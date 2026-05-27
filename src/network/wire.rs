//! Length-prefixed bincode wire protocol for P2P communication.
//!
//! Frame format: [4-byte length (u32 big-endian)][bincode payload]
//! Maximum frame size: 8MB (prevents memory exhaustion; responses capped at 50 blocks ~400KB)

use crate::block::types::{Block, BlockHeader, MasternodeTierCounts};
use crate::network::message::NetworkMessage;
use crate::types::Hash256;
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum allowed frame size (8MB)
/// Block range responses are capped at MAX_BLOCKS_PER_RESPONSE (50 blocks, ~400 KB
/// compressed). 8MB gives a large safety margin while keeping per-peer buffer
/// allocation predictable on small VPS nodes.
pub const MAX_FRAME_SIZE: u32 = 8 * 1024 * 1024;

/// Serialize a NetworkMessage and write it as a length-prefixed frame.
pub async fn write_message<W: AsyncWrite + Unpin>(
    writer: &mut W,
    message: &NetworkMessage,
) -> Result<(), String> {
    let payload =
        bincode::serialize(message).map_err(|e| format!("Failed to serialize message: {}", e))?;

    let len = payload.len() as u32;
    if len > MAX_FRAME_SIZE {
        return Err(format!(
            "Message too large: {} bytes (max: {})",
            len, MAX_FRAME_SIZE
        ));
    }

    writer
        .write_all(&len.to_be_bytes())
        .await
        .map_err(|e| format!("Failed to write frame length: {}", e))?;

    writer
        .write_all(&payload)
        .await
        .map_err(|e| format!("Failed to write frame payload: {}", e))?;

    writer
        .flush()
        .await
        .map_err(|e| format!("Failed to flush: {}", e))?;

    Ok(())
}

/// Pre-serialize a NetworkMessage into a length-prefixed frame (for broadcast efficiency).
pub fn serialize_frame(message: &NetworkMessage) -> Result<Vec<u8>, String> {
    let payload =
        bincode::serialize(message).map_err(|e| format!("Failed to serialize message: {}", e))?;

    let len = payload.len() as u32;
    if len > MAX_FRAME_SIZE {
        return Err(format!(
            "Message too large: {} bytes (max: {})",
            len, MAX_FRAME_SIZE
        ));
    }

    let mut frame = Vec::with_capacity(4 + payload.len());
    frame.extend_from_slice(&len.to_be_bytes());
    frame.extend_from_slice(&payload);
    Ok(frame)
}

// ---------------------------------------------------------------------------
// Legacy block types for backward-compatible wire deserialization.
//
// Pre-v1.3 nodes serialize BlockHeader WITHOUT the `total_fees: u64` field.
// Bincode is not self-describing, so the missing 8 bytes cause deserialization
// to fail for every Block-containing message. These types let us try a second
// deserialization pass and convert to the current format with total_fees = 0.
// ---------------------------------------------------------------------------

/// BlockHeader as serialized by pre-v1.3 nodes (no total_fees).
#[derive(Serialize, Deserialize)]
struct LegacyBlockHeader {
    pub version: u32,
    pub height: u64,
    pub previous_hash: Hash256,
    pub merkle_root: Hash256,
    pub timestamp: i64,
    pub block_reward: u64,
    pub leader: String,
    pub attestation_root: Hash256,
    pub masternode_tiers: MasternodeTierCounts,
    pub vrf_proof: Vec<u8>,
    pub vrf_output: Hash256,
    pub vrf_score: u64,
    pub active_masternodes_bitmap: Vec<u8>,
    pub liveness_recovery: Option<bool>,
    pub producer_signature: Vec<u8>,
    // NO total_fees — that's the whole point
}

/// Block as serialized by pre-v1.3 nodes.
#[allow(deprecated)]
#[derive(Serialize, Deserialize)]
struct LegacyBlock {
    pub header: LegacyBlockHeader,
    pub transactions: Vec<crate::types::Transaction>,
    pub masternode_rewards: Vec<(String, u64)>,
    pub time_attestations: Vec<crate::block::types::TimeAttestation>,
    pub consensus_participants_bitmap: Vec<u8>,
    pub liveness_recovery: Option<bool>,
}

impl From<LegacyBlockHeader> for BlockHeader {
    fn from(h: LegacyBlockHeader) -> Self {
        BlockHeader {
            version: h.version,
            height: h.height,
            previous_hash: h.previous_hash,
            merkle_root: h.merkle_root,
            timestamp: h.timestamp,
            block_reward: h.block_reward,
            leader: h.leader,
            attestation_root: h.attestation_root,
            masternode_tiers: h.masternode_tiers,
            vrf_proof: h.vrf_proof,
            vrf_output: h.vrf_output,
            vrf_score: h.vrf_score,
            active_masternodes_bitmap: h.active_masternodes_bitmap,
            liveness_recovery: h.liveness_recovery,
            producer_signature: h.producer_signature,
            total_fees: 0,
            treasury_balance: 0,
        }
    }
}

#[allow(deprecated)]
impl From<LegacyBlock> for Block {
    fn from(b: LegacyBlock) -> Self {
        Block {
            header: b.header.into(),
            transactions: b.transactions,
            masternode_rewards: b.masternode_rewards,
            time_attestations: b.time_attestations,
            consensus_participants_bitmap: b.consensus_participants_bitmap,
            liveness_recovery: b.liveness_recovery,
        }
    }
}

/// Deserialize a single pre-v1.3 Block (without total_fees) from raw bytes.
/// Used by blockchain.rs for sled storage migration.
pub fn deserialize_legacy_block(data: &[u8]) -> Option<Block> {
    let legacy: LegacyBlock = bincode::deserialize(data).ok()?;
    Some(legacy.into())
}

/// Bincode variant indices for NetworkMessage variants that contain Block.
/// These are determined by declaration order in the enum (0-indexed).
const VARIANT_BLOCKS_RESPONSE: u32 = 7;
const VARIANT_GENESIS_ANNOUNCEMENT: u32 = 9;
const VARIANT_BLOCK_ANNOUNCEMENT: u32 = 20;
const VARIANT_BLOCK_RESPONSE: u32 = 23;
const VARIANT_BLOCK_RANGE_RESPONSE: u32 = 50;
const VARIANT_TIMELOCK_BLOCK_PROPOSAL: u32 = 61;

/// Try to deserialize a payload as a pre-v1.3 message (blocks without total_fees).
/// Returns None if the payload isn't a Block-containing variant or legacy parse also fails.
fn try_legacy_deserialize(payload: &[u8]) -> Option<NetworkMessage> {
    if payload.len() < 4 {
        return None;
    }
    let variant_idx = u32::from_le_bytes(payload[..4].try_into().ok()?);
    let data = &payload[4..];

    match variant_idx {
        VARIANT_BLOCKS_RESPONSE => {
            let blocks: Vec<LegacyBlock> = bincode::deserialize(data).ok()?;
            let count = blocks.len();
            let msg = NetworkMessage::BlocksResponse(blocks.into_iter().map(Into::into).collect());
            tracing::info!(
                "🔄 Migrated pre-v1.3 BlocksResponse ({} blocks) from legacy wire format",
                count
            );
            Some(msg)
        }
        VARIANT_GENESIS_ANNOUNCEMENT => {
            let block: LegacyBlock = bincode::deserialize(data).ok()?;
            tracing::info!("🔄 Migrated pre-v1.3 GenesisAnnouncement from legacy wire format");
            Some(NetworkMessage::GenesisAnnouncement(block.into()))
        }
        VARIANT_BLOCK_ANNOUNCEMENT => {
            let block: LegacyBlock = bincode::deserialize(data).ok()?;
            tracing::info!("🔄 Migrated pre-v1.3 BlockAnnouncement from legacy wire format");
            Some(NetworkMessage::BlockAnnouncement(block.into()))
        }
        VARIANT_BLOCK_RESPONSE => {
            let block: LegacyBlock = bincode::deserialize(data).ok()?;
            tracing::info!("🔄 Migrated pre-v1.3 BlockResponse from legacy wire format");
            Some(NetworkMessage::BlockResponse(block.into()))
        }
        VARIANT_BLOCK_RANGE_RESPONSE => {
            let blocks: Vec<LegacyBlock> = bincode::deserialize(data).ok()?;
            let count = blocks.len();
            let msg =
                NetworkMessage::BlockRangeResponse(blocks.into_iter().map(Into::into).collect());
            tracing::info!(
                "🔄 Migrated pre-v1.3 BlockRangeResponse ({} blocks) from legacy wire format",
                count
            );
            Some(msg)
        }
        VARIANT_TIMELOCK_BLOCK_PROPOSAL => {
            let block: LegacyBlock = bincode::deserialize(data).ok()?;
            tracing::info!("🔄 Migrated pre-v1.3 TimeLockBlockProposal from legacy wire format");
            Some(NetworkMessage::TimeLockBlockProposal {
                block: block.into(),
            })
        }
        _ => None,
    }
}

/// Read a length-prefixed frame and deserialize into a NetworkMessage.
/// Returns Ok(None) on clean EOF (connection closed).
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<NetworkMessage>, String> {
    // Read 4-byte length prefix
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e)
            if e.kind() == std::io::ErrorKind::UnexpectedEof
                || e.kind() == std::io::ErrorKind::ConnectionReset =>
        {
            return Ok(None)
        }
        Err(e) => return Err(format!("Failed to read frame length: {}", e)),
    }

    let len = u32::from_be_bytes(len_buf);

    if len > MAX_FRAME_SIZE {
        return Err(format!(
            "Frame too large: {} bytes (max: {})",
            len, MAX_FRAME_SIZE
        ));
    }

    // Read payload
    let mut payload = vec![0u8; len as usize];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|e| format!("Failed to read frame payload: {}", e))?;

    let message: NetworkMessage = match bincode::deserialize(&payload) {
        Ok(msg) => msg,
        Err(e) => {
            // Try legacy deserialization for pre-v1.3 Block messages (missing total_fees)
            if let Some(legacy_msg) = try_legacy_deserialize(&payload) {
                legacy_msg
            } else if payload.len() > 1000 {
                tracing::warn!(
                    "⚠️ Failed to deserialize large message ({} bytes) — peer may be running incompatible code: {}",
                    payload.len(),
                    e
                );
                NetworkMessage::UnknownMessage
            } else {
                tracing::debug!(
                    "⚠️ Received unrecognized message ({} bytes), skipping: {}",
                    payload.len(),
                    e
                );
                NetworkMessage::UnknownMessage
            }
        }
    };

    Ok(Some(message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_roundtrip() {
        let msg = NetworkMessage::Ping {
            nonce: 42,
            timestamp: 1234567890,
            height: Some(100),
        };

        let mut buf = Vec::new();
        write_message(&mut buf, &msg).await.unwrap();

        let mut cursor = std::io::Cursor::new(buf);
        let result = read_message(&mut cursor).await.unwrap().unwrap();

        match result {
            NetworkMessage::Ping {
                nonce,
                timestamp,
                height,
            } => {
                assert_eq!(nonce, 42);
                assert_eq!(timestamp, 1234567890);
                assert_eq!(height, Some(100));
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[tokio::test]
    async fn test_eof_returns_none() {
        let mut cursor = std::io::Cursor::new(Vec::<u8>::new());
        let result = read_message(&mut cursor).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_oversized_frame_rejected() {
        let len = (MAX_FRAME_SIZE + 1).to_be_bytes();
        let mut cursor = std::io::Cursor::new(len.to_vec());
        let result = read_message(&mut cursor).await;
        assert!(result.is_err());
    }

    /// Verify that a BlocksResponse serialized WITHOUT total_fees (pre-v1.3 format)
    /// is correctly deserialized via the legacy fallback path.
    #[tokio::test]
    #[allow(deprecated)]
    async fn test_legacy_blocks_response_migration() {
        use crate::block::types::MasternodeTierCounts;
        use crate::types::{OutPoint, Transaction, TxInput, TxOutput};

        // Build a legacy block (no total_fees in header)
        let legacy_block = LegacyBlock {
            header: LegacyBlockHeader {
                version: 1,
                height: 42,
                previous_hash: [1u8; 32],
                merkle_root: [2u8; 32],
                timestamp: 1700000000,
                block_reward: 10_000_000_000,
                leader: "test_leader".into(),
                attestation_root: [0u8; 32],
                masternode_tiers: MasternodeTierCounts::default(),
                vrf_proof: vec![],
                vrf_output: [0u8; 32],
                vrf_score: 0,
                active_masternodes_bitmap: vec![],
                liveness_recovery: Some(false),
                producer_signature: vec![0u8; 64],
            },
            transactions: vec![Transaction {
                version: 1,
                inputs: vec![TxInput {
                    previous_output: OutPoint {
                        txid: [3u8; 32],
                        vout: 0,
                    },
                    script_sig: vec![1, 2, 3],
                    sequence: 0xFFFFFFFF,
                }],
                outputs: vec![TxOutput {
                    value: 5_000_000_000,
                    script_pubkey: vec![4, 5, 6],
                }],
                lock_time: 0,
                timestamp: 1700000000,
                special_data: None,
                encrypted_memo: None,
            }],
            masternode_rewards: vec![("test_addr".into(), 100_000_000)],
            time_attestations: vec![],
            consensus_participants_bitmap: vec![],
            liveness_recovery: Some(false),
        };

        // Serialize as a legacy BlocksResponse: [u32 variant=7][Vec<LegacyBlock>]
        let mut payload = Vec::new();
        payload.extend_from_slice(&VARIANT_BLOCKS_RESPONSE.to_le_bytes());
        let blocks_data = bincode::serialize(&vec![legacy_block]).unwrap();
        payload.extend_from_slice(&blocks_data);

        // Wrap in a length-prefixed frame (as wire.rs expects)
        let frame_len = payload.len() as u32;
        let mut frame = Vec::new();
        frame.extend_from_slice(&frame_len.to_be_bytes());
        frame.extend_from_slice(&payload);

        // read_message should succeed via the legacy fallback
        let mut cursor = std::io::Cursor::new(frame);
        let result = read_message(&mut cursor).await.unwrap().unwrap();

        match result {
            NetworkMessage::BlocksResponse(blocks) => {
                assert_eq!(blocks.len(), 1);
                assert_eq!(blocks[0].header.height, 42);
                assert_eq!(blocks[0].header.total_fees, 0);
                assert_eq!(blocks[0].transactions.len(), 1);
                assert_eq!(
                    blocks[0].masternode_rewards,
                    vec![("test_addr".to_string(), 100_000_000)]
                );
            }
            other => panic!("Expected BlocksResponse, got {:?}", other),
        }
    }

    /// Verify that single-block legacy deserialization works (for sled migration).
    #[test]
    #[allow(deprecated)]
    fn test_legacy_single_block_deserialization() {
        use crate::block::types::MasternodeTierCounts;

        let legacy_block = LegacyBlock {
            header: LegacyBlockHeader {
                version: 1,
                height: 100,
                previous_hash: [5u8; 32],
                merkle_root: [6u8; 32],
                timestamp: 1700000000,
                block_reward: 10_000_000_000,
                leader: "leader".into(),
                attestation_root: [0u8; 32],
                masternode_tiers: MasternodeTierCounts::default(),
                vrf_proof: vec![],
                vrf_output: [0u8; 32],
                vrf_score: 0,
                active_masternodes_bitmap: vec![],
                liveness_recovery: Some(false),
                producer_signature: vec![],
            },
            transactions: vec![],
            masternode_rewards: vec![],
            time_attestations: vec![],
            consensus_participants_bitmap: vec![],
            liveness_recovery: Some(false),
        };

        let data = bincode::serialize(&legacy_block).unwrap();
        let block = deserialize_legacy_block(&data).expect("Legacy block should deserialize");
        assert_eq!(block.header.height, 100);
        assert_eq!(block.header.total_fees, 0);
    }
}
