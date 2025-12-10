# TIME Coin Node - Quick Start Guide

## 🚀 5-Minute Setup

### Step 1: Install Rust

**Windows:**
```powershell
# Download and run rustup-init.exe from https://rustup.rs/
# Or use winget:
winget install Rustlang.Rustup
```

**Linux/macOS:**
```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

### Step 2: Clone and Build

```bash
# Clone repository
git clone <repository-url>
cd timecoin

# Build (takes ~1 minute first time)
cargo build --release
```

### Step 3: Run

```bash
cargo run --release
```

You should see:
```
🚀 TIME Coin Protocol Node v0.1.0
=====================================

✓ Initialized 3 masternodes
✓ Added initial UTXO with 5000 TIME
✓ Consensus engine initialized

📡 Starting demo transaction...
✅ Transaction finalized instantly!

🎉 TIME Coin node is running!
```

## 🎮 Interactive Demo

### Test Transaction Processing

The node automatically processes a demo transaction on startup showing:
1. UTXO locking
2. BFT voting simulation
3. Instant finality
4. New UTXO creation

### Check Network Server

The node listens on `0.0.0.0:24100` for P2P connections.

Test with telnet:
```bash
telnet localhost 24100
```

Send a JSON message:
```json
{"GetUTXOSet": null}
```

### Generate Blocks

Edit `src/main.rs` to change block height:
```rust
let block = consensus.generate_deterministic_block(2, timestamp).await;
```

## 🧪 Development Commands

```bash
# Run in debug mode (faster compile, slower runtime)
cargo run

# Run tests
cargo test

# Check code without building
cargo check

# Format code
cargo fmt

# Lint code
cargo clippy

# Build documentation
cargo doc --open
```

## 🔧 Configuration

### Modify Masternodes

Edit `src/main.rs`:
```rust
let masternodes = vec![
    Masternode {
        address: "your-address".to_string(),
        collateral: 10_000,  // Change tier
        public_key: keypair.verifying_key(),
        tier: MasternodeTier::Silver,  // Bronze/Silver/Gold
    },
    // Add more masternodes...
];
```

### Change Network Port

Edit `src/main.rs`:
```rust
NetworkServer::new("0.0.0.0:9999", utxo_mgr, consensus).await
```

### Adjust Rate Limits

Edit `src/network/rate_limiter.rs`:
```rust
limits: [
    ("tx".to_string(), (Duration::from_secs(1), 2000)), // 2000 tx/s
    ("utxo_query".to_string(), (Duration::from_secs(1), 200)), // 200/s
    // ...
]
```

## 📊 Monitoring

### Check UTXO State

Add to your code:
```rust
let state = utxo_manager.get_state(&outpoint).await;
println!("UTXO state: {:?}", state);
```

### View Block Details

```rust
println!("Block height: {}", block.header.height);
println!("Block hash: {}", hex::encode(block.hash()));
println!("Transactions: {}", block.transactions.len());
println!("Rewards: {:?}", block.masternode_rewards);
```

### Network Statistics

Add logging in `src/network/server.rs`:
```rust
println!("Connected peers: {}", peers.read().await.len());
println!("Active subscriptions: {}", subs.read().await.len());
```

## 🐛 Troubleshooting

### Port Already in Use

If you see "Address already in use":
```bash
# Windows
netstat -ano | findstr :24100
taskkill /F /PID <process_id>

# Linux/macOS
lsof -i :24100
kill -9 <process_id>
```

Or change the port in `main.rs`.

### Build Errors

```bash
# Update Rust
rustup update

# Clean build
cargo clean
cargo build --release
```

### Slow Performance

Make sure you're using release mode:
```bash
cargo build --release
./target/release/time-coin-node
```

Debug builds are 10-100x slower.

## 📦 Binary Distribution

After building, the executable is at:
- **Windows**: `target\release\time-coin-node.exe`
- **Linux/macOS**: `target/release/time-coin-node`

Copy this file to distribute the node (no Rust required to run).

## 🌐 Network Testing

### Start Multiple Nodes

Terminal 1:
```bash
cargo run --release
```

Terminal 2:
```bash
# Edit port first, then:
cargo run --release
```

### Send Test Messages

Use `netcat` or write a client:
```bash
echo '{"UTXOStateQuery":[]}' | nc localhost 24100
```

## 📚 Next Steps

1. **Read the docs**: Check `README.md` and `IMPLEMENTATION.md`
2. **Explore the code**: Start with `src/main.rs`
3. **Modify parameters**: Try different masternode tiers
4. **Add features**: Implement persistent storage
5. **Test scenarios**: Write unit tests

## 💡 Example Modifications

### Add Your Own Transaction

```rust
let my_tx = Transaction {
    version: 1,
    inputs: vec![
        TxInput {
            previous_output: OutPoint { txid: my_txid, vout: 0 },
            script_sig: vec![],
            sequence: 0xFFFFFFFF,
        }
    ],
    outputs: vec![
        TxOutput { value: 1000, script_pubkey: vec![] }
    ],
    lock_time: 0,
    timestamp: now(),
};

consensus.process_transaction(my_tx).await?;
```

### Query UTXO Set

```rust
let utxos = utxo_manager.utxo_set.read().await;
for (outpoint, utxo) in utxos.iter() {
    println!("UTXO: {:?} = {} TIME", outpoint, utxo.value);
}
```

### Generate Multiple Blocks

```rust
for height in 1..=10 {
    let block = consensus.generate_deterministic_block(height, timestamp).await;
    println!("Generated block #{}: {}", height, hex::encode(block.hash()));
}
```

## 🎯 Success Criteria

You know everything is working when:
- ✅ Build completes without errors
- ✅ Node starts and shows initialization messages
- ✅ Demo transaction gets finalized
- ✅ Block is generated with correct structure
- ✅ Network server starts listening
- ✅ No panic or crash messages

## 📞 Getting Help

- Check `README.md` for detailed documentation
- Review `IMPLEMENTATION.md` for technical details
- Examine error messages carefully
- Use `RUST_BACKTRACE=1` for detailed errors:
  ```bash
  RUST_BACKTRACE=1 cargo run
  ```

## 🎉 Congratulations!

You now have a working TIME Coin Protocol node!

Explore the codebase, experiment with parameters, and start building your blockchain application.

---

**Happy Coding! 🚀**
