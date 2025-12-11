#![allow(dead_code)]
#![allow(clippy::redundant_closure)]

use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::io;
use std::path::Path;
use std::sync::Arc;
use thiserror::Error;
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};

#[derive(Error, Debug)]
pub enum TlsError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),
    #[error("TLS error: {0}")]
    Tls(#[from] rustls::Error),
    #[error("Invalid certificate")]
    InvalidCertificate,
    #[error("Invalid private key")]
    InvalidPrivateKey,
}

/// TLS configuration for secure peer-to-peer communication
#[derive(Clone)]
pub struct TlsConfig {
    pub client_config: Arc<rustls::ClientConfig>,
    pub server_config: Arc<rustls::ServerConfig>,
}

impl TlsConfig {
    /// Create a new TLS configuration with self-signed certificates for P2P
    /// In production, use proper certificate management
    pub fn new_self_signed() -> Result<Self, TlsError> {
        // Generate self-signed certificate for development/testing
        let cert = rcgen::generate_simple_self_signed(vec!["timecoin.local".to_string()])
            .map_err(|_| TlsError::InvalidCertificate)?;

        let cert_der = CertificateDer::from(cert.cert.der().to_vec());
        let key_der = PrivateKeyDer::try_from(cert.key_pair.serialize_der())
            .map_err(|_| TlsError::InvalidPrivateKey)?;

        // Server config (accepts connections)
        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert_der.clone()], key_der.clone_key())
            .map_err(|e| TlsError::Tls(e))?;

        // Client config (initiates connections)
        // For P2P, we'll use a permissive verifier that accepts self-signed certs
        let mut client_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertVerifier))
            .with_no_client_auth();

        // Enable session resumption for performance
        client_config.resumption = rustls::client::Resumption::default();

        Ok(Self {
            client_config: Arc::new(client_config),
            server_config: Arc::new(server_config),
        })
    }

    /// Load TLS configuration from PEM files
    pub fn from_pem_files(cert_path: &Path, key_path: &Path) -> Result<Self, TlsError> {
        let cert_file = std::fs::File::open(cert_path)?;
        let key_file = std::fs::File::open(key_path)?;

        let mut cert_reader = io::BufReader::new(cert_file);
        let mut key_reader = io::BufReader::new(key_file);

        let certs: Vec<CertificateDer> =
            rustls_pemfile::certs(&mut cert_reader).collect::<Result<Vec<_>, _>>()?;

        let key =
            rustls_pemfile::private_key(&mut key_reader)?.ok_or(TlsError::InvalidPrivateKey)?;

        let server_config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs.clone(), key.clone_key())
            .map_err(|e| TlsError::Tls(e))?;

        let mut client_config = rustls::ClientConfig::builder()
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCertVerifier))
            .with_no_client_auth();

        client_config.resumption = rustls::client::Resumption::default();

        Ok(Self {
            client_config: Arc::new(client_config),
            server_config: Arc::new(server_config),
        })
    }

    /// Create a TLS connector for outbound connections
    pub fn connector(&self) -> TlsConnector {
        TlsConnector::from(self.client_config.clone())
    }

    /// Create a TLS acceptor for inbound connections
    pub fn acceptor(&self) -> TlsAcceptor {
        TlsAcceptor::from(self.server_config.clone())
    }

    /// Wrap a TCP stream with TLS as a client
    pub async fn connect_client(
        &self,
        stream: TcpStream,
        domain: &str,
    ) -> Result<tokio_rustls::client::TlsStream<TcpStream>, TlsError> {
        let connector = self.connector();
        let domain = rustls::pki_types::ServerName::try_from(domain)
            .map_err(|_| TlsError::InvalidCertificate)?
            .to_owned();

        Ok(connector.connect(domain, stream).await?)
    }

    /// Wrap a TCP stream with TLS as a server
    pub async fn accept_server(
        &self,
        stream: TcpStream,
    ) -> Result<tokio_rustls::server::TlsStream<TcpStream>, TlsError> {
        let acceptor = self.acceptor();
        Ok(acceptor.accept(stream).await?)
    }
}

/// Custom certificate verifier for P2P networks with self-signed certificates
/// WARNING: This accepts ANY certificate - suitable for P2P but not client-server
#[derive(Debug)]
struct AcceptAnyCertVerifier;

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCertVerifier {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer,
        _intermediates: &[CertificateDer],
        _server_name: &rustls::pki_types::ServerName,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        // For P2P networks, we accept any certificate
        // Message-level signatures provide authentication
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn verify_tls13_signature(
        &self,
        _message: &[u8],
        _cert: &CertificateDer,
        _dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        Ok(rustls::client::danger::HandshakeSignatureValid::assertion())
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        vec![
            rustls::SignatureScheme::RSA_PKCS1_SHA256,
            rustls::SignatureScheme::ECDSA_NISTP256_SHA256,
            rustls::SignatureScheme::ED25519,
        ]
    }
}

/// Helper to check if a stream is TLS-wrapped or plain TCP
pub enum SecureStream {
    ClientTls(tokio_rustls::client::TlsStream<TcpStream>),
    ServerTls(tokio_rustls::server::TlsStream<TcpStream>),
    Plain(TcpStream),
}

impl SecureStream {
    pub async fn read_buf<B>(&mut self, buf: &mut B) -> io::Result<usize>
    where
        B: bytes::BufMut,
        Self: Unpin,
    {
        match self {
            SecureStream::ClientTls(stream) => {
                use tokio::io::AsyncReadExt;
                let mut temp = vec![0u8; 8192];
                let n = stream.read(&mut temp).await?;
                buf.put_slice(&temp[..n]);
                Ok(n)
            }
            SecureStream::ServerTls(stream) => {
                use tokio::io::AsyncReadExt;
                let mut temp = vec![0u8; 8192];
                let n = stream.read(&mut temp).await?;
                buf.put_slice(&temp[..n]);
                Ok(n)
            }
            SecureStream::Plain(stream) => {
                use tokio::io::AsyncReadExt;
                let mut temp = vec![0u8; 8192];
                let n = stream.read(&mut temp).await?;
                buf.put_slice(&temp[..n]);
                Ok(n)
            }
        }
    }

    pub async fn write_all(&mut self, buf: &[u8]) -> io::Result<()>
    where
        Self: Unpin,
    {
        match self {
            SecureStream::ClientTls(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.write_all(buf).await
            }
            SecureStream::ServerTls(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.write_all(buf).await
            }
            SecureStream::Plain(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.write_all(buf).await
            }
        }
    }

    pub async fn flush(&mut self) -> io::Result<()>
    where
        Self: Unpin,
    {
        match self {
            SecureStream::ClientTls(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.flush().await
            }
            SecureStream::ServerTls(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.flush().await
            }
            SecureStream::Plain(stream) => {
                use tokio::io::AsyncWriteExt;
                stream.flush().await
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_self_signed_config() {
        let config = TlsConfig::new_self_signed();
        assert!(config.is_ok());
    }

    #[tokio::test]
    async fn test_tls_handshake() {
        let config = TlsConfig::new_self_signed().unwrap();

        // Create a test listener
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        // Server task
        let server_config = config.clone();
        tokio::spawn(async move {
            if let Ok((stream, _)) = listener.accept().await {
                let _ = server_config.accept_server(stream).await;
            }
        });

        // Client task
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let stream = TcpStream::connect(addr).await.unwrap();
        let result = config.connect_client(stream, "timecoin.local").await;

        assert!(result.is_ok());
    }
}
