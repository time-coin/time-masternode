/// Secure transport layer for P2P communication
/// Combines TLS encryption and message-level signing
// TODO: Remove once integrated into server/client
#[allow(dead_code)]
use crate::network::message::NetworkMessage;
use crate::network::signed_message::{SignedMessage, SignedMessageError};
use crate::network::tls::{SecureStream, TlsConfig};
use ed25519_dalek::{SigningKey, VerifyingKey};
use std::sync::Arc;
use thiserror::Error;
use tokio::io::BufReader;
use tokio::net::TcpStream;

#[derive(Error, Debug)]
pub enum SecureTransportError {
    #[error("TLS error: {0}")]
    Tls(#[from] crate::network::tls::TlsError),
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
    #[error("Signed message error: {0}")]
    SignedMessage(#[from] SignedMessageError),
    #[error("Serialization error: {0}")]
    Serialization(String),
    #[error("Invalid message timestamp (age: {0}s, max: {1}s)")]
    InvalidTimestamp(i64, i64),
    #[error("Handshake failed: {0}")]
    HandshakeFailed(String),
}

/// Configuration for secure transport
#[allow(dead_code)]
#[derive(Clone)]
pub struct SecureTransportConfig {
    pub enable_tls: bool,
    pub enable_signing: bool,
    pub message_max_age_seconds: i64,
    pub tls_config: Option<Arc<TlsConfig>>,
    pub signing_key: Option<Arc<SigningKey>>,
}

#[allow(dead_code)]
impl SecureTransportConfig {
    /// Create a new secure transport config with defaults
    pub fn new(enable_tls: bool, enable_signing: bool) -> Result<Self, SecureTransportError> {
        let tls_config = if enable_tls {
            Some(Arc::new(TlsConfig::new_self_signed()?))
        } else {
            None
        };

        let signing_key = if enable_signing {
            let mut rng = rand::rngs::OsRng;
            let key_bytes: [u8; 32] = rand::Rng::gen(&mut rng);
            Some(Arc::new(SigningKey::from_bytes(&key_bytes)))
        } else {
            None
        };

        Ok(Self {
            enable_tls,
            enable_signing,
            message_max_age_seconds: 300, // 5 minutes default
            tls_config,
            signing_key,
        })
    }

    /// Create config with provided keys
    pub fn with_keys(
        enable_tls: bool,
        enable_signing: bool,
        tls_config: Option<Arc<TlsConfig>>,
        signing_key: Option<Arc<SigningKey>>,
        message_max_age_seconds: i64,
    ) -> Self {
        Self {
            enable_tls,
            enable_signing,
            message_max_age_seconds,
            tls_config,
            signing_key,
        }
    }

    pub fn verifying_key(&self) -> Option<VerifyingKey> {
        self.signing_key.as_ref().map(|k| k.verifying_key())
    }
}

/// Secure transport layer that wraps TCP with optional TLS and message signing
#[allow(dead_code)]
pub struct SecureTransport {
    config: SecureTransportConfig,
}

#[allow(dead_code)]
impl SecureTransport {
    pub fn new(config: SecureTransportConfig) -> Self {
        Self { config }
    }

    /// Wrap an outbound TCP connection (client side)
    pub async fn wrap_client(
        &self,
        stream: TcpStream,
        domain: &str,
    ) -> Result<SecureConnection, SecureTransportError> {
        let inner = if self.config.enable_tls {
            if let Some(ref tls_config) = self.config.tls_config {
                tracing::debug!("🔒 Establishing TLS connection to {}", domain);
                let tls_stream = tls_config.connect_client(stream, domain).await?;
                SecureStream::ClientTls(tls_stream)
            } else {
                return Err(SecureTransportError::Tls(
                    crate::network::tls::TlsError::InvalidCertificate,
                ));
            }
        } else {
            SecureStream::Plain(stream)
        };

        Ok(SecureConnection {
            inner,
            config: self.config.clone(),
        })
    }

    /// Wrap an inbound TCP connection (server side)
    pub async fn wrap_server(
        &self,
        stream: TcpStream,
    ) -> Result<SecureConnection, SecureTransportError> {
        let inner = if self.config.enable_tls {
            if let Some(ref tls_config) = self.config.tls_config {
                tracing::debug!("🔒 Accepting TLS connection");
                let tls_stream = tls_config.accept_server(stream).await?;
                SecureStream::ServerTls(tls_stream)
            } else {
                return Err(SecureTransportError::Tls(
                    crate::network::tls::TlsError::InvalidCertificate,
                ));
            }
        } else {
            SecureStream::Plain(stream)
        };

        Ok(SecureConnection {
            inner,
            config: self.config.clone(),
        })
    }
}

/// A secure connection with encryption and authentication
#[allow(dead_code)]
pub struct SecureConnection {
    inner: SecureStream,
    config: SecureTransportConfig,
}

#[allow(dead_code)]
impl SecureConnection {
    /// Send a message over the secure connection
    pub async fn send_message(
        &mut self,
        message: NetworkMessage,
    ) -> Result<(), SecureTransportError> {
        let payload = if self.config.enable_signing {
            if let Some(ref signing_key) = self.config.signing_key {
                let timestamp = chrono::Utc::now().timestamp();
                let signed_message = SignedMessage::new(message, signing_key, timestamp)?;
                serde_json::to_string(&("signed", signed_message))
                    .map_err(|e| SecureTransportError::Serialization(e.to_string()))?
            } else {
                return Err(SecureTransportError::SignedMessage(
                    SignedMessageError::MissingSignature,
                ));
            }
        } else {
            serde_json::to_string(&("plain", message))
                .map_err(|e| SecureTransportError::Serialization(e.to_string()))?
        };

        self.inner
            .write_all(format!("{}\n", payload).as_bytes())
            .await?;
        self.inner.flush().await?;

        Ok(())
    }

    /// Receive a message from the secure connection
    pub async fn receive_message(&mut self) -> Result<NetworkMessage, SecureTransportError> {
        let mut buffer = String::new();

        // Read line based on stream type
        let bytes_read = match &mut self.inner {
            SecureStream::ClientTls(stream) => {
                use tokio::io::AsyncBufReadExt;
                let mut reader = BufReader::new(stream);
                reader.read_line(&mut buffer).await?
            }
            SecureStream::ServerTls(stream) => {
                use tokio::io::AsyncBufReadExt;
                let mut reader = BufReader::new(stream);
                reader.read_line(&mut buffer).await?
            }
            SecureStream::Plain(stream) => {
                use tokio::io::AsyncBufReadExt;
                let mut reader = BufReader::new(stream);
                reader.read_line(&mut buffer).await?
            }
        };

        if bytes_read == 0 {
            return Err(SecureTransportError::Io(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "Connection closed",
            )));
        }

        // Remove newline
        let line = buffer.trim_end();

        // Try to parse as (type, payload) tuple
        let (msg_type, message): (String, serde_json::Value) = serde_json::from_str(line)
            .map_err(|e| SecureTransportError::Serialization(e.to_string()))?;

        match msg_type.as_str() {
            "signed" => {
                if !self.config.enable_signing {
                    tracing::warn!("⚠️ Received signed message but signing is disabled");
                }

                let signed_message: SignedMessage = serde_json::from_value(message)
                    .map_err(|e| SecureTransportError::Serialization(e.to_string()))?;

                // Verify signature
                signed_message.verify()?;

                // Verify timestamp
                if !signed_message.is_timestamp_valid(self.config.message_max_age_seconds) {
                    let age = (chrono::Utc::now().timestamp() - signed_message.timestamp).abs();
                    return Err(SecureTransportError::InvalidTimestamp(
                        age,
                        self.config.message_max_age_seconds,
                    ));
                }

                Ok(signed_message.payload)
            }
            "plain" => {
                let message: NetworkMessage = serde_json::from_value(message)
                    .map_err(|e| SecureTransportError::Serialization(e.to_string()))?;
                Ok(message)
            }
            _ => Err(SecureTransportError::Serialization(format!(
                "Unknown message type: {}",
                msg_type
            ))),
        }
    }

    /// Perform handshake - exchange capabilities and verify connection
    pub async fn handshake(
        &mut self,
        our_magic: [u8; 4],
        our_network: &str,
    ) -> Result<(), SecureTransportError> {
        // Send handshake
        let handshake = NetworkMessage::Handshake {
            magic: our_magic,
            protocol_version: 1,
            network: our_network.to_string(),
        };

        self.send_message(handshake).await?;

        // Receive handshake response
        match self.receive_message().await? {
            NetworkMessage::Handshake {
                magic,
                protocol_version: _,
                network,
            } => {
                if magic != our_magic {
                    return Err(SecureTransportError::HandshakeFailed(format!(
                        "Magic bytes mismatch: expected {:?}, got {:?}",
                        our_magic, magic
                    )));
                }
                if network != our_network {
                    return Err(SecureTransportError::HandshakeFailed(format!(
                        "Network mismatch: expected {}, got {}",
                        our_network, network
                    )));
                }
                Ok(())
            }
            _ => Err(SecureTransportError::HandshakeFailed(
                "Expected Handshake message".to_string(),
            )),
        }
    }
}

/// Convenience wrapper for reading/writing with automatic buffering
#[allow(dead_code)]
pub struct SecureReader {
    reader: BufReader<SecureStream>,
    config: SecureTransportConfig,
}

#[allow(dead_code)]
pub struct SecureWriter {
    config: SecureTransportConfig,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_crypto() {
        // Initialize crypto provider for rustls
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn test_config_creation() {
        init_crypto();
        let config = SecureTransportConfig::new(false, false);
        assert!(config.is_ok());

        let config = SecureTransportConfig::new(true, true);
        assert!(config.is_ok());
    }

    #[tokio::test]
    async fn test_tls_transport() {
        init_crypto();
        let config = SecureTransportConfig::new(true, false).unwrap();
        let transport = SecureTransport::new(config);

        // This is a smoke test - actual connection testing requires network setup
        assert!(transport.config.enable_tls);
    }
}
