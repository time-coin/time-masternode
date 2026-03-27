//! Minimal async HTTP client using tokio + rustls.
//!
//! Replaces reqwest to avoid its ~235 transitive dependencies (ICU unicode stack, hyper, tower, etc.).
//! Supports only what the daemon and CLI tools actually need:
//! - GET/POST requests with optional JSON body
//! - Basic HTTP authentication
//! - TLS with self-signed certificate acceptance
//! - Configurable timeouts

use crate::network::tls::AcceptAnyCertVerifier;
use rustls::pki_types::ServerName;
use rustls::RootCertStore;
use serde::de::DeserializeOwned;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// A lightweight HTTP client.
#[derive(Clone)]
pub struct HttpClient {
    timeout: Duration,
    accept_invalid_certs: bool,
}

/// HTTP response from a request.
pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl HttpResponse {
    /// Deserialize the response body as JSON.
    pub fn json<T: DeserializeOwned>(&self) -> Result<T, String> {
        serde_json::from_slice(&self.body).map_err(|e| {
            format!(
                "JSON parse error: {} (body: {})",
                e,
                String::from_utf8_lossy(&self.body[..self.body.len().min(200)])
            )
        })
    }

    /// Get the response body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// Check if the response status is 2xx.
    pub fn is_success(&self) -> bool {
        (200..300).contains(&self.status)
    }

    /// Get the value of a header (case-insensitive).
    pub fn header(&self, name: &str) -> Option<&str> {
        let name_lower = name.to_ascii_lowercase();
        self.headers
            .iter()
            .find(|(k, _)| k.to_ascii_lowercase() == name_lower)
            .map(|(_, v)| v.as_str())
    }
}

impl Default for HttpClient {
    fn default() -> Self {
        Self::new()
    }
}

impl HttpClient {
    pub fn new() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            accept_invalid_certs: false,
        }
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub fn with_accept_invalid_certs(mut self, accept: bool) -> Self {
        self.accept_invalid_certs = accept;
        self
    }

    /// Send a GET request and return the response.
    pub async fn get(&self, url: &str) -> Result<HttpResponse, String> {
        self.request("GET", url, None, None).await
    }

    /// Send a POST request with a JSON body and return the response.
    pub async fn post_json(
        &self,
        url: &str,
        body: &impl serde::Serialize,
        basic_auth: Option<(&str, &str)>,
    ) -> Result<HttpResponse, String> {
        let json = serde_json::to_vec(body).map_err(|e| e.to_string())?;
        self.request("POST", url, Some(json), basic_auth).await
    }

    async fn request(
        &self,
        method: &str,
        url: &str,
        body: Option<Vec<u8>>,
        basic_auth: Option<(&str, &str)>,
    ) -> Result<HttpResponse, String> {
        let mut current_url = url.to_string();
        let max_redirects = 5;

        for _ in 0..max_redirects {
            let (scheme, host, port, path) = parse_url(&current_url)?;
            let use_tls = scheme == "https";

            // Build the HTTP request
            let mut request = format!(
                "{} {} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n",
                method, path, host
            );

            if let Some(ref body) = body {
                request.push_str(&format!(
                    "Content-Type: application/json\r\nContent-Length: {}\r\n",
                    body.len()
                ));
            }

            if let Some((user, pass)) = basic_auth {
                let credentials = base64::Engine::encode(
                    &base64::engine::general_purpose::STANDARD,
                    format!("{}:{}", user, pass),
                );
                request.push_str(&format!("Authorization: Basic {}\r\n", credentials));
            }

            request.push_str("\r\n");

            // Connect with timeout
            let addr = format!("{}:{}", host, port);
            let response = tokio::time::timeout(self.timeout, async {
                let stream = TcpStream::connect(&addr)
                    .await
                    .map_err(|e| format!("TCP connect to {}: {}", addr, e))?;

                if use_tls {
                    self.do_tls_request(stream, &host, &request, body.as_deref())
                        .await
                } else {
                    self.do_plain_request(stream, &request, body.as_deref())
                        .await
                }
            })
            .await
            .map_err(|_| {
                format!(
                    "Request to {} timed out after {:?}",
                    current_url, self.timeout
                )
            })??;

            // Follow redirects (301, 302, 303, 307, 308), but only within
            // the same scheme. Never upgrade HTTP → HTTPS automatically;
            // callers that want TLS should use an https:// URL directly.
            if matches!(response.status, 301 | 302 | 303 | 307 | 308) {
                if let Some(location) = extract_location_header(&response) {
                    let same_scheme = location.starts_with(&format!("{}://", scheme));
                    let relative = !location.contains("://");
                    if same_scheme || relative {
                        current_url = location;
                        continue;
                    }
                }
            }

            return Ok(response);
        }

        Err(format!("Too many redirects for {}", url))
    }

    async fn do_plain_request(
        &self,
        mut stream: TcpStream,
        request: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, String> {
        stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        if let Some(body) = body {
            stream.write_all(body).await.map_err(|e| e.to_string())?;
        }
        stream.flush().await.map_err(|e| e.to_string())?;

        let mut response_buf = Vec::new();
        stream
            .read_to_end(&mut response_buf)
            .await
            .map_err(|e| e.to_string())?;
        parse_http_response(&response_buf)
    }

    async fn do_tls_request(
        &self,
        stream: TcpStream,
        host: &str,
        request: &str,
        body: Option<&[u8]>,
    ) -> Result<HttpResponse, String> {
        let mut tls_config = if self.accept_invalid_certs {
            // P2P / RPC nodes use self-signed certs — skip verification.
            rustls::ClientConfig::builder()
                .dangerous()
                .with_custom_certificate_verifier(Arc::new(AcceptAnyCertVerifier))
                .with_no_client_auth()
        } else {
            // External HTTPS (e.g. peer discovery API) — use webpki root CAs
            // so the TLS handshake looks like a normal client to CDN hosts.
            let mut root_store = RootCertStore::empty();
            root_store.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
            rustls::ClientConfig::builder()
                .with_root_certificates(root_store)
                .with_no_client_auth()
        };
        tls_config.alpn_protocols = vec![b"http/1.1".to_vec()];

        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config));
        let server_name = ServerName::try_from(host.to_owned())
            .map_err(|e| format!("Invalid server name '{}': {}", host, e))?;

        let mut tls_stream = connector
            .connect(server_name, stream)
            .await
            .map_err(|e| format!("TLS handshake with {}: {}", host, e))?;

        tls_stream
            .write_all(request.as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        if let Some(body) = body {
            tls_stream
                .write_all(body)
                .await
                .map_err(|e| e.to_string())?;
        }
        tls_stream.flush().await.map_err(|e| e.to_string())?;

        let mut response_buf = Vec::new();
        tls_stream
            .read_to_end(&mut response_buf)
            .await
            .map_err(|e| e.to_string())?;
        parse_http_response(&response_buf)
    }
}

/// Extract the Location header value from an HTTP redirect response.
fn extract_location_header(response: &HttpResponse) -> Option<String> {
    response.header("location").map(|s| s.to_string())
}

/// Parse a URL into (scheme, host, port, path).
fn parse_url(url: &str) -> Result<(String, String, u16, String), String> {
    let (scheme, rest) = url
        .split_once("://")
        .ok_or_else(|| format!("Invalid URL (no scheme): {}", url))?;

    let default_port: u16 = match scheme {
        "https" => 443,
        "http" => 80,
        _ => return Err(format!("Unsupported scheme: {}", scheme)),
    };

    let (host_port, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };

    let (host, port) = match host_port.rsplit_once(':') {
        Some((h, p)) => {
            let port = p
                .parse::<u16>()
                .map_err(|_| format!("Invalid port in URL: {}", url))?;
            (h.to_string(), port)
        }
        None => (host_port.to_string(), default_port),
    };

    Ok((scheme.to_string(), host, port, path.to_string()))
}

/// Parse raw HTTP response bytes into status code + headers + body.
fn parse_http_response(data: &[u8]) -> Result<HttpResponse, String> {
    // Find end of headers
    let header_end = data
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or("Malformed HTTP response: no header terminator")?;

    let header_str =
        std::str::from_utf8(&data[..header_end]).map_err(|_| "Non-UTF8 HTTP headers")?;

    let mut lines = header_str.lines();

    // Parse status line: "HTTP/1.1 200 OK"
    let status_line = lines.next().ok_or("Empty HTTP response")?;

    let status = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| format!("Malformed status line: {}", status_line))?
        .parse::<u16>()
        .map_err(|_| format!("Invalid status code in: {}", status_line))?;

    // Parse headers
    let mut headers = Vec::new();
    let mut is_chunked = false;
    for line in lines {
        if let Some((name, value)) = line.split_once(':') {
            let name = name.trim().to_string();
            let value = value.trim().to_string();
            if name.eq_ignore_ascii_case("transfer-encoding")
                && value.to_ascii_lowercase().contains("chunked")
            {
                is_chunked = true;
            }
            headers.push((name, value));
        }
    }

    let body_start = header_end + 4;

    let body = if is_chunked {
        decode_chunked(&data[body_start..])?
    } else {
        data[body_start..].to_vec()
    };

    Ok(HttpResponse {
        status,
        headers,
        body,
    })
}

/// Decode chunked transfer encoding.
fn decode_chunked(data: &[u8]) -> Result<Vec<u8>, String> {
    let mut result = Vec::new();
    let mut pos = 0;

    loop {
        if pos >= data.len() {
            break;
        }

        // Find the end of the chunk size line
        let line_end = data[pos..]
            .windows(2)
            .position(|w| w == b"\r\n")
            .ok_or("Malformed chunked encoding: no CRLF after chunk size")?;

        let size_str = std::str::from_utf8(&data[pos..pos + line_end])
            .map_err(|_| "Non-UTF8 chunk size")?
            .trim();

        let chunk_size = usize::from_str_radix(size_str, 16)
            .map_err(|_| format!("Invalid chunk size: {}", size_str))?;

        if chunk_size == 0 {
            break; // Final chunk
        }

        let chunk_start = pos + line_end + 2; // skip past CRLF
        let chunk_end = chunk_start + chunk_size;

        if chunk_end > data.len() {
            // Partial chunk — take what we have
            result.extend_from_slice(&data[chunk_start..]);
            break;
        }

        result.extend_from_slice(&data[chunk_start..chunk_end]);
        pos = chunk_end + 2; // skip trailing CRLF
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_url() {
        let (scheme, host, port, path) = parse_url("https://time-coin.io/api/peers").unwrap();
        assert_eq!(scheme, "https");
        assert_eq!(host, "time-coin.io");
        assert_eq!(port, 443);
        assert_eq!(path, "/api/peers");

        let (scheme, host, port, path) = parse_url("http://127.0.0.1:24101").unwrap();
        assert_eq!(scheme, "http");
        assert_eq!(host, "127.0.0.1");
        assert_eq!(port, 24101);
        assert_eq!(path, "/");
    }

    #[test]
    fn test_parse_http_response() {
        let response = b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n[\"1.2.3.4\"]";
        let parsed = parse_http_response(response).unwrap();
        assert_eq!(parsed.status, 200);
        assert!(parsed.is_success());
        let body: Vec<String> = parsed.json().unwrap();
        assert_eq!(body, vec!["1.2.3.4"]);
    }

    #[test]
    fn test_parse_chunked_response() {
        let response = b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let parsed = parse_http_response(response).unwrap();
        assert_eq!(parsed.text(), "hello world");
    }

    #[test]
    fn test_http_401() {
        let response = b"HTTP/1.1 401 Unauthorized\r\n\r\n";
        let parsed = parse_http_response(response).unwrap();
        assert_eq!(parsed.status, 401);
        assert!(!parsed.is_success());
    }
}
