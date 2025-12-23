# ✅ Project Renamed to `timed`

## 🎉 Rename Complete!

Your TIME Coin Protocol node has been successfully renamed from `time-coin-node` to **`timed`** (TIME Daemon).

---

## 📦 New Binary Name

```bash
target/release/timed.exe
```

---

## 🚀 How to Run

### Development
```bash
cargo run --release
```

### Direct Binary
```bash
./target/release/timed.exe
```

### With Options
```bash
./target/release/timed.exe --help
./target/release/timed.exe --verbose
./target/release/timed.exe --config custom.toml
```

---

## 📊 Build Status

### ✅ All Checks Passed

**Formatting**: ✓ PASSED  
**Linting**: ✓ PASSED (7 intentional warnings)  
**Compilation**: ✓ SUCCESS  
**Release Build**: ✓ COMPLETE (7.61s)

---

## 📁 Updated Files

The following files have been updated with the new name:

1. ✅ `Cargo.toml` - Package name changed to `timed`
2. ✅ `src/main.rs` - Command name updated
3. ✅ `Dockerfile` - Binary name updated
4. ✅ `timecoin-node.service` - Service references updated
5. ✅ `install.sh` - Installation script updated

---

## 🔧 Service Name (Linux)

### Systemd Commands
```bash
# Start
sudo systemctl start timed

# Stop
sudo systemctl stop timed

# Status
sudo systemctl status timed

# Logs
sudo journalctl -u timed -f
```

---

## 🐳 Docker

### Build
```bash
docker build -t timed .
```

### Run
```bash
docker run -d -p 24100:24100 -p 24101:24101 --name timed timed
```

---

## 💡 Why "timed"?

**timed** = **TIME Daemon**

Following Unix daemon naming convention (like `systemd`, `sshd`, `httpd`):
- Short and memorable
- Clear it's a daemon/service
- Easy to type
- Professional naming

---

## 🎯 Quick Reference

| What | Command |
|------|---------|
| Run node | `cargo run --release` |
| Binary location | `./target/release/timed.exe` |
| Help | `timed --help` |
| Generate config | `timed --generate-config` |
| Verbose mode | `timed --verbose` |
| Custom port | `timed --listen-addr 0.0.0.0:9999` |

---

## ✅ Everything Still Works!

All functionality remains the same:
- ✅ UTXO State Machine
- ✅ BFT Consensus
- ✅ Deterministic Blocks
- ✅ P2P Network
- ✅ Configuration System
- ✅ Logging
- ✅ Docker Support
- ✅ Systemd Integration

Only the **name** has changed!

---

**Your TIME Coin daemon (`timed`) is ready to run!** 🚀

```bash
cargo run --release
```
