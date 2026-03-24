# TIME Coin RPC Connection Analysis - For IP 170.64.3.187

## PROBLEM STATEMENT
Client at IP 170.64.3.187 is connecting to RPC with plain HTTP (no TLS), experiencing:
- "tls handshake eof" errors
- "InvalidContentType" errors  
- RPC now requires TLS

## EXECUTIVE SUMMARY

The TIME Coin codebase has **THREE client tools** that connect to the RPC server:

1. **time-cli.rs** (CLI tool) - Lines 519-590
2. **time-dashboard.rs** (Dashboard/TUI tool) - Lines 223-363
3. **RPC Server** (main.rs) - Listens on fixed ports with CONFIGURABLE TLS

**KEY FINDING**: Both clients default to **HTTPS with fallback to HTTP**, but both make hard-coded connection attempts to plain HTTP first in network auto-detection. This is likely where the problematic client code lives.

---

## 1. RPC PORT CONFIGURATION (network_type.rs)

The RPC server listens on **fixed port pairs** with NO separate HTTPS port:

### MAINNET
- **RPC Port: 24001** (HTTPS when TLS enabled, HTTP when disabled)
- P2P Port: 24000
- WebSocket Port: 24002

### TESTNET  
- **RPC Port: 24101** (HTTPS when TLS enabled, HTTP when disabled)
- P2P Port: 24100
- WebSocket Port: 24102

**IMPORTANT**: The RPC server uses the **SAME PORT for both HTTP and HTTPS**. It selects protocol based on pctls config (TLS enabled by default).

---

## 2. RPC SERVER SETUP (main.rs, src/rpc/server.rs)

### Server Configuration (main.rs)
- Config parameter: pctls (default: **TRUE** - TLS enabled)
- Optional cert/key: pctlscert and pctlskey
- If no cert specified: **Self-signed certificate auto-generated**
- Transport mode logged at startup as "TLS" or "plain"

### Server Implementation (src/rpc/server.rs)
- Uses 	okio_rustls::TlsAcceptor for TLS when enabled
- Handles both TLS and plain TCP streams
- TLS handshake happens BEFORE HTTP parsing (line 352-362)
- If TLS handshake fails: Connection dropped with error logged
  - "RPC TLS handshake failed from {addr}: {error}"

### Key Code (server.rs lines 352-365):
\\\ust
if let Some(acceptor) = tls {
    match acceptor.accept(socket).await {
        Ok(tls_stream) => {
            if let Err(e) = Self::handle_connection(tls_stream, handler, &auth).await {
                eprintln!("RPC TLS error: {}", e);
            }
        }
        Err(e) => eprintln!("RPC TLS handshake failed from {}: {}", addr, e),
    }
} else if let Err(e) = Self::handle_connection(socket, handler, &auth).await {
    eprintln!("RPC error: {}", e);
}
\\\

---

## 3. TIME-CLI RPC CONNECTION CODE (src/bin/time-cli.rs)

### Client Setup (lines 519-590)
\\\ust
// Build reqwest client: always accept self-signed certs
let tls_client = reqwest::Client::builder()
    .danger_accept_invalid_certs(true)  // <-- IMPORTANT: Accepts self-signed certs
    .build()
    .unwrap_or_else(|_| Client::new());
\\\

### Auto-Detection Logic (lines 531-578)
**CRITICAL ISSUE**: Auto-detection prefers HTTPS but tries specific schemes based on config:

1. Reads pctls setting from 	ime.conf (line 534)
2. If pctls=1 (default): Tries ["https", "http"] in order
3. If pctls=0: Tries ["http"] only
4. Attempts both testnet (24101) and mainnet (24001)

**PROBLEMATIC BEHAVIOR**:
- Line 533: Hardcoded IPs: "127.0.0.1:24101" (testnet) and "127.0.0.1:24001" (mainnet)
- Only connects to localhost - cannot reach remote RPC servers
- If no local server responds: Falls back to https://127.0.0.1:{port} by default

### RPC Call Code (lines 522-589)
\\\ust
let mut req = tls_client
    .post(&url)
    .json(&serde_json::json!({...}))
    .timeout(std::time::Duration::from_secs(2));
if !user.is_empty() && !pass.is_empty() {
    req = req.basic_auth(&user, Some(&pass));
}
\\\

### Key Auto-Detection Code (lines 572-578)
\\\ust
detected.unwrap_or_else(|| {
    let testnet = args.testnet;
    let port = if testnet { 24101 } else { 24001 };
    let use_tls = read_conf_rpctls(testnet);  // <-- Reads rpctls from time.conf
    let scheme = if use_tls { "https" } else { "http" };
    (format!("{}://127.0.0.1:{}", scheme, port), testnet)
})
\\\

### TLS Config Read (lines 448-478)
\\\ust
fn read_conf_rpctls(testnet: bool) -> bool {
    // Parse time.conf in ~/.timecoin/testnet or ~/.timecoin
    // Look for line: rpctls=0 (to disable TLS)
    // Default: rpctls = true (TLS enabled)
    
    let contents = match std::fs::read_to_string(&conf_path) {
        Ok(c) => c,
        Err(_) => return true, // default: TLS on
    };
    let mut rpctls = true; // server default
    for line in contents.lines() {
        if let Some((key, value)) = line.split_once('=') {
            if key.trim() == "rpctls" {
                rpctls = value.trim() != "0";
            }
        }
    }
    rpctls
}
\\\

---

## 4. TIME-DASHBOARD RPC CONNECTION CODE (src/bin/time-dashboard.rs)

### Client Setup (lines 223-239)
\\\ust
client: Client::builder()
    .timeout(Duration::from_secs(3))
    .build()
    .unwrap_or_default(),
\\\

**CRITICAL ISSUE**: Dashboard does NOT accept self-signed certs! No .danger_accept_invalid_certs() call.

### RPC Call Method (lines 311-363)
\\\ust
async fn rpc_call<T: for<'de> Deserialize<'de>>(
    &self,
    method: &str,
    params: Vec<serde_json::Value>,
) -> Result<T, Box<dyn Error>> {
    let mut req = self.client.post(&self.rpc_url).json(&request);
    if !self.rpc_user.is_empty() && !self.rpc_pass.is_empty() {
        req = req.basic_auth(&self.rpc_user, Some(&self.rpc_pass));
    }
    let response = req.send().await?;
    let response_text = response.text().await?;
    let rpc_value: serde_json::Value = serde_json::from_str(&response_text)?;
    
    if let Some(error) = rpc_value.get("error") {
        if !error.is_null() {
            return Err(format!("RPC error: {}", error).into());
        }
    }
    ...
}
\\\

### Auto-Detection (lines 1023-1059)
\\\ust
async fn detect_network() -> (String, bool) {
    let client = Client::new();
    
    let ports: Vec<(&str, bool)> = vec![
        ("http://127.0.0.1:24101", true),   // testnet - PLAIN HTTP ONLY
        ("http://127.0.0.1:24001", false),  // mainnet - PLAIN HTTP ONLY
    ];
    
    for (url, is_testnet) in ports {
        // Attempts only HTTP, no HTTPS fallback
        // Only connects to localhost
        ...
    }
    
    // Default to testnet HTTP
    ("http://127.0.0.1:24101".to_string(), true)
}
\\\

**CRITICAL VULNERABILITY**: 
- Lines 1027-1030: **Hard-coded HTTP-only URLs**
- No TLS support at all
- No support for remote RPC servers
- Will fail immediately with "tls handshake eof" if server requires TLS

---

## 5. WALLET.RS (src/wallet.rs)

This file contains wallet encryption/storage logic, NOT RPC connection code.
- Uses AES-256-GCM with Argon2 for key derivation
- Does not make RPC calls itself

---

## 6. SUMMARY TABLE: RPC CONNECTION METHODS

| Component | File | Lines | HTTP Support | HTTPS Support | Auto-Detect | Localhost Only | Issue |
|-----------|------|-------|--------------|---------------|-------------|----------------|-------|
| time-cli | src/bin/time-cli.rs | 519-590 | ✅ (fallback) | ✅ (primary) | ✅ (respects rpctls config) | ✅ | None - correctly implements TLS-first |
| time-dashboard | src/bin/time-dashboard.rs | 1023-1059 | ✅ ONLY | ❌ NO | ❌ (hardcoded HTTP) | ✅ | **BROKEN: No HTTPS support** |
| RPC Server | src/main.rs + src/rpc/server.rs | Lines 2700-2900 | ✅ (if rpctls=0) | ✅ (default: rpctls=1) | N/A | N/A | Works as designed |

---

## 7. ROOT CAUSE FOR IP 170.64.3.187

The client at **170.64.3.187** is likely using **time-dashboard.rs** because:

1. **time-dashboard** hardcodes HTTP-only connections (lines 1027-1030)
2. Does not implement TLS/HTTPS at all
3. Sends unencrypted HTTP request to a server expecting HTTPS
4. Server TLS acceptor tries to perform TLS handshake
5. Client sends HTTP bytes instead of TLS handshake
6. Result: "tls handshake eof" (server sees EOF on HTTPS, client sent HTTP)

Alternative: Client may be using old/misconfigured **time-cli** with pctls=0 in time.conf, but then connecting to a server with pctls=1.

---

## 8. CONFIGURATION REQUIREMENTS

### Server Side (time.conf)
To enable TLS (default):
\\\
rpctls=1
rpctlscert=/path/to/cert.pem
rpctlskey=/path/to/key.pem
# If no cert/key specified: auto-generates self-signed
\\\

To disable TLS:
\\\
rpctls=0
\\\

RPC port: **24001** (mainnet) or **24101** (testnet) - SAME port for HTTP and HTTPS

### Client Side (time.conf)
time-cli respects server's TLS setting via:
\\\
rpctls=1  # or 0 to disable
\\\

---

## 9. FIX FOR IP 170.64.3.187 CLIENT

### Option 1: Update time-dashboard to support HTTPS
Replace lines 1027-1030 in **src/bin/time-dashboard.rs**:

FROM:
\\\ust
let ports: Vec<(&str, bool)> = vec![
    ("http://127.0.0.1:24101", true),   // testnet
    ("http://127.0.0.1:24001", false),  // mainnet
];
\\\

TO:
\\\ust
let ports: Vec<(&str, bool)> = vec![
    ("https://127.0.0.1:24101", true),   // testnet - try HTTPS first
    ("http://127.0.0.1:24101", true),    // testnet - fallback to HTTP
    ("https://127.0.0.1:24001", false),  // mainnet - try HTTPS first
    ("http://127.0.0.1:24001", false),   // mainnet - fallback to HTTP
];
\\\

Also add self-signed cert support (line 1024):
\\\ust
let client = Client::builder()
    .danger_accept_invalid_certs(true)  // Accept self-signed RPC certs
    .build()
    .unwrap_or_else(|_| Client::new());
\\\

### Option 2: Use time-cli instead of time-dashboard
time-cli correctly supports both HTTP and HTTPS (lines 522-590)

### Option 3: Disable TLS on server (NOT RECOMMENDED)
\\\
rpctls=0
\\\

---

## 10. KEY FINDINGS

1. **RPC Server TLS is ENABLED by default** - Uses same port (24001/24101) for both
2. **Self-signed certificates are auto-generated** if no cert/key provided
3. **time-cli is correct** - Supports HTTPS with self-signed cert acceptance
4. **time-dashboard is BROKEN** - Only supports HTTP, no HTTPS at all
5. **No separate HTTPS port** - TLS negotiation happens on standard RPC port
6. **No frontend/UI files** - Only CLI/TUI tools exist in codebase
7. **localhost-only by default** - Both clients cannot reach remote RPC servers

