# Port Configuration Explanation

**Date:** December 20, 2025

## Short Answer

**7000 for testing is CORRECT for local development** because the default testnet port is **24100**. Using 7000 avoids conflicts with actual testnet nodes that might be running.

---

## Official Port Configuration

### Mainnet
- **P2P Port:** 24000
- **RPC Port:** 24001

### Testnet
- **P2P Port:** 24100
- **RPC Port:** 24101

**Source:** `src/network_type.rs` lines 17-28

---

## Why Use 7000 for Local Testing?

### Option 1: Use Official Testnet Port (24100) ❌
```bash
.\target\release\timed --node-id 1 --p2p-port 24100
.\target\release\timed --node-id 2 --p2p-port 24100  # FAILS - port already in use!
```

**Problem:** Can't run multiple nodes on same port on same machine

### Option 2: Use Arbitrary Local Ports (7000+) ✅
```bash
.\target\release\timed --node-id 1 --p2p-port 7000
.\target\release\timed --node-id 2 --p2p-port 7001
.\target\release\timed --node-id 3 --p2p-port 7002
```

**Advantages:**
- No conflicts with each other
- No conflicts with actual testnet (which runs on 24100)
- Easy to test locally
- Clean separation: local testing vs testnet

---

## Port Usage by Environment

### Local Development (Your Machine)
```
Node 1: 7000 (arbitrary, local-only)
Node 2: 7001 (arbitrary, local-only)
Node 3: 7002 (arbitrary, local-only)
RPC:    9999 (arbitrary, local-only)
```

**These ports are:**
- ✅ For local testing only
- ✅ Not connected to real network
- ✅ Arbitrary choice (7000 is just convenient)
- ✅ Can be any available port

### Testnet (Real Network)
```
All nodes: 24100 (standard testnet P2P port)
All nodes: 24101 (standard testnet RPC port)
```

**These ports are:**
- ✅ Standard across all testnet nodes
- ✅ Used for actual network communication
- ✅ Must be consistent across testnet
- ✅ Should not change

### Mainnet (Future - Live Network)
```
All nodes: 24000 (standard mainnet P2P port)
All nodes: 24001 (standard mainnet RPC port)
```

---

## How Nodes Know Their Port

### Local Testing (From Command Line)
```bash
.\target\release\timed --node-id 1 --p2p-port 7000
                                      ^^^^^^^^^^^^^^^^
                                      Explicitly specified
```

### Testnet/Mainnet (From Config File)
```toml
# config.toml
[network]
type = "testnet"  # Determines ports: 24100 (P2P), 24101 (RPC)

# OR explicitly:
p2p_port = 24100
rpc_port = 24101
```

---

## Default Behavior

### When You DON'T Specify a Port
```bash
.\target\release\timed --node-id 1
```

**Result:**
- Reads `config.toml` to determine network type
- If network type is Testnet → Uses port 24100 (default testnet)
- If network type is Mainnet → Uses port 24000 (default mainnet)

**Code:** `src/network/client.rs`
```rust
pub fn new(
    // ...
    network_type: NetworkType,
) -> Self {
    let p2p_port = network_type.default_p2p_port();
    // For testnet: 24100
    // For mainnet: 24000
}
```

---

## For Real Testnet Deployment

### What You WILL Do
```bash
# Default - uses port 24100 automatically
systemctl start timed

# OR explicitly with config
./timed --config config.testnet.toml
# (config.testnet.toml specifies network=testnet → port 24100)

# OR explicitly with command line
./timed --p2p-port 24100  # (usually not needed, it's the default)
```

### What Happens
All testnet nodes automatically use **port 24100** because:
1. They all read the same config
2. Config specifies `network = "testnet"`
3. Testnet type defaults to port 24100
4. All nodes on port 24100 discover each other

---

## Summary Table

| Scenario | Port | Source | Reason |
|----------|------|--------|--------|
| Local Node 1 | 7000 | CLI arg | Testing, avoid conflicts |
| Local Node 2 | 7001 | CLI arg | Testing, avoid conflicts |
| Local Node 3 | 7002 | CLI arg | Testing, avoid conflicts |
| Testnet Node | 24100 | Config/Default | Standard testnet port |
| Mainnet Node | 24000 | Config/Default | Standard mainnet port |

---

## Best Practices

### For Local Testing
✅ Use arbitrary ports (7000-8000 range)  
✅ Change for each node to avoid conflicts  
✅ Use different from real network ports (24000, 24100)  

### For Testnet
✅ Use default ports (24100 for P2P)  
✅ Don't specify port (let config handle it)  
✅ Keep consistent across all nodes  

### For Mainnet
✅ Use default ports (24000 for P2P)  
✅ Must be accessible from internet  
✅ Keep consistent across all nodes  

---

## How Nodes Discover Each Other

### Local Testing (7000, 7001, 7002)
Nodes discover hardcoded bootstrap peers with specified ports
```
Node 2 connects to: 127.0.0.1:7000 (Node 1)
Node 3 connects to: 127.0.0.1:7000 (Node 1)
```

### Real Testnet (all on 24100)
Nodes discover other testnet nodes automatically
```
Node A connects to: 165.232.154.150:24100
Node A connects to: 178.128.199.144:24100
Node A connects to: 69.167.168.176:24100
```

**Key Point:** Testnet peers are discovered by DNS/bootstrap, so they all being on 24100 works fine.

---

## Updated Testing Instructions

Your local test commands are correct:

```bash
# Local testing - 7000 is appropriate
.\target\release\timed --node-id 1 --p2p-port 7000
.\target\release\timed --node-id 2 --p2p-port 7001
.\target\release\timed --node-id 3 --p2p-port 7002
```

For testnet deployment, you'll use the default:
```bash
# Testnet - 24100 is automatic
systemctl start timed
# (no port specified, uses config → port 24100)
```

---

## References

**Source Code:**
- `src/network_type.rs` - Port definitions
- `src/network/client.rs` - Default port assignment
- `src/config.rs` - Config file handling

**Files:**
- `config.toml` - Local dev config (can specify any port)
- `config.testnet.toml` - Testnet config (should use 24100)
- `config.mainnet.toml` - Mainnet config (should use 24000)

---

**Summary:** Using 7000+ for local testing is the right approach. For real deployment, the system automatically uses the correct ports (24100 for testnet, 24000 for mainnet) based on the config file.
