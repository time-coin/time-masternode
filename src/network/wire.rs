//! Length-prefixed bincode wire protocol for P2P communication.
//!
//! Frame format: [4-byte length (u32 big-endian)][bincode payload]
//! Maximum frame size: 4MB (prevents memory exhaustion attacks)

use crate::network::message::NetworkMessage;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// Maximum allowed frame size (4MB)
pub const MAX_FRAME_SIZE: u32 = 4 * 1024 * 1024;

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

/// Read a length-prefixed frame and deserialize into a NetworkMessage.
/// Returns Ok(None) on clean EOF (connection closed).
pub async fn read_message<R: AsyncRead + Unpin>(
    reader: &mut R,
) -> Result<Option<NetworkMessage>, String> {
    // Read 4-byte length prefix
    let mut len_buf = [0u8; 4];
    match reader.read_exact(&mut len_buf).await {
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
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

    let message: NetworkMessage = bincode::deserialize(&payload)
        .map_err(|e| format!("Failed to deserialize message: {}", e))?;

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
}
