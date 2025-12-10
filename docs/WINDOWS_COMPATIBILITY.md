# Windows Compatibility Guide

## Current Status
✅ **The TIME Coin daemon is cross-platform compatible!**

All dependencies and code use platform-agnostic Rust crates that work on Windows, Linux, and macOS.

---

## Prerequisites for Windows

### 1. Install Rust
```powershell
# Download and run rustup-init.exe from:
# https://rustup.rs/

# Or use winget:
winget install Rustlang.Rustup
```

### 2. Install Visual Studio Build Tools
The Rust compiler on Windows requires the Microsoft C++ build tools:

```powershell
# Option A: Install Visual Studio Community (includes build tools)
winget install Microsoft.VisualStudio.2022.Community

# Option B: Install just the build tools
winget install Microsoft.VisualStudio.2022.BuildTools
```

During installation, select:
- ✅ Desktop development with C++
- ✅ Windows 10/11 SDK

### 3. Verify Installation
```powershell
rustc --version
cargo --version
```

---

## Building on Windows

### Clone and Build
```powershell
# Clone the repository
git clone https://github.com/yourusername/timecoin.git
cd timecoin

# Build the project
cargo build --release

# Run the daemon
.\target\release\timed.exe

# Or run directly
cargo run --release --bin timed
```

---

## Windows-Specific Considerations

### Data Directory
On Windows, the default data directory is:
```
%APPDATA%\timecoin\
```

For testnet:
```
%APPDATA%\timecoin\testnet\
```

### Configuration File
The daemon looks for `config.toml` in:
1. Current working directory
2. `%APPDATA%\timecoin\config.toml`

### Firewall Configuration
You'll need to allow the daemon through Windows Firewall:

```powershell
# Allow P2P port (24100 for testnet, 23100 for mainnet)
New-NetFirewallRule -DisplayName "TIME Coin P2P" -Direction Inbound -LocalPort 24100 -Protocol TCP -Action Allow

# Allow RPC port (24101 for testnet, 23101 for mainnet)
New-NetFirewallRule -DisplayName "TIME Coin RPC" -Direction Inbound -LocalPort 24101 -Protocol TCP -Action Allow
```

Or use the GUI:
1. Open Windows Defender Firewall
2. Click "Advanced settings"
3. Click "Inbound Rules" → "New Rule"
4. Select "Port" → TCP → Specific local ports: 24100, 24101
5. Allow the connection
6. Name the rule "TIME Coin"

---

## Platform-Agnostic Dependencies

All our dependencies work cross-platform:

| Crate | Windows Support | Notes |
|-------|-----------------|-------|
| `tokio` | ✅ Yes | Uses IOCP on Windows |
| `sled` | ✅ Yes | Cross-platform embedded DB |
| `ed25519-dalek` | ✅ Yes | Pure Rust crypto |
| `blake3` | ✅ Yes | SIMD optimizations for x86 |
| `reqwest` | ✅ Yes | Uses native-tls or rustls |
| `sysinfo` | ✅ Yes | Provides system stats on all platforms |
| `dirs` | ✅ Yes | Provides correct paths per OS |
| `hostname` | ✅ Yes | Works on Windows |

---

## Running as a Windows Service

### Option 1: Using NSSM (Non-Sucking Service Manager)
```powershell
# Download NSSM from: https://nssm.cc/download

# Install as service
nssm install TimeCoin "C:\path\to\timed.exe"
nssm set TimeCoin AppDirectory "C:\path\to\timecoin"
nssm set TimeCoin DisplayName "TIME Coin Daemon"
nssm set TimeCoin Description "TIME Coin blockchain node"

# Start the service
nssm start TimeCoin
```

### Option 2: Using sc.exe (built-in)
```powershell
sc.exe create TimeCoin binPath= "C:\path\to\timed.exe" start= auto
sc.exe start TimeCoin
```

---

## Windows-Specific Testing

### Test on Different Windows Versions
- ✅ Windows 10 (21H2+)
- ✅ Windows 11
- ✅ Windows Server 2019/2022

### Test Network Connectivity
```powershell
# Test P2P connection
Test-NetConnection -ComputerName time-coin.io -Port 24100

# Check listening ports
netstat -an | findstr "24100 24101"
```

### Performance Monitoring
```powershell
# Monitor CPU/Memory
Get-Process timed | Select-Object CPU,WorkingSet64,Handles

# Use Task Manager for GUI monitoring
```

---

## Known Platform Differences

### Path Separators
✅ **Handled automatically** - Rust's `PathBuf` and `std::path` handle Windows backslashes correctly.

### Line Endings
✅ **No issues** - All file I/O uses binary mode or handles CRLF/LF automatically.

### Case Sensitivity
✅ **Handled** - All file operations use proper case handling for Windows' case-insensitive filesystem.

### Maximum Path Length
⚠️ **Potential issue** - Windows has a 260-character path limit by default.

**Solution**: Enable long path support in Windows 10+:
```powershell
# Run as Administrator
New-ItemProperty -Path "HKLM:\SYSTEM\CurrentControlSet\Control\FileSystem" -Name "LongPathsEnabled" -Value 1 -PropertyType DWORD -Force
```

Or in Group Policy:
1. Run `gpedit.msc`
2. Navigate to: Computer Configuration → Administrative Templates → System → Filesystem
3. Enable "Enable Win32 long paths"

---

## Development on Windows

### Recommended Tools
- **IDE**: Visual Studio Code with rust-analyzer extension
- **Terminal**: Windows Terminal or PowerShell 7+
- **Git**: Git for Windows

### Hot Reload Development
```powershell
# Install cargo-watch
cargo install cargo-watch

# Auto-rebuild on changes
cargo watch -x "run --bin timed"
```

### Cross-Platform Testing
Use WSL2 (Windows Subsystem for Linux) for testing Linux builds:

```powershell
# Install WSL2
wsl --install

# Build and test in Linux environment
wsl
cd /mnt/c/path/to/timecoin
cargo build --release
```

---

## Troubleshooting Windows Issues

### Issue: "error: linker 'link.exe' not found"
**Solution**: Install Visual Studio Build Tools (see Prerequisites)

### Issue: Firewall blocking connections
**Solution**: Add firewall rules (see Firewall Configuration above)

### Issue: Permission denied when binding ports
**Solution**: Run PowerShell as Administrator or use ports > 1024

### Issue: Slow compilation times
**Solution**: Exclude the project directory from Windows Defender:
```powershell
Add-MpPreference -ExclusionPath "C:\path\to\timecoin"
```

### Issue: Network connectivity problems
**Solution**: Check Windows Firewall and ensure P2P port is accessible:
```powershell
Test-NetConnection -ComputerName localhost -Port 24100
```

---

## Performance Optimizations for Windows

### 1. Use Release Builds
```powershell
cargo build --release
```

### 2. Enable LTO (Link-Time Optimization)
Already configured in `Cargo.toml`:
```toml
[profile.release]
lto = true
codegen-units = 1
```

### 3. Pin to Performance Cores (Windows 11)
```powershell
# Set process affinity to performance cores
$process = Get-Process timed
$process.ProcessorAffinity = 0xFF  # Adjust mask for your CPU
```

### 4. Increase Process Priority
```powershell
# Set to high priority (requires admin)
$process = Get-Process timed
$process.PriorityClass = "High"
```

---

## Continuous Integration for Windows

### GitHub Actions Example
```yaml
name: Windows Build

on: [push, pull_request]

jobs:
  windows:
    runs-on: windows-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo build --release --verbose
      - run: cargo test --release --verbose
      - run: cargo clippy -- -D warnings
```

---

## Distribution

### Installer Options
1. **MSI Installer**: Use WiX Toolset
2. **Portable ZIP**: Just bundle the exe + config
3. **Chocolatey Package**: For package managers
4. **winget Package**: Submit to Windows Package Manager

### Example: Create Portable Distribution
```powershell
# Create distribution folder
New-Item -ItemType Directory -Path .\dist\windows
Copy-Item .\target\release\timed.exe .\dist\windows\
Copy-Item .\config.toml .\dist\windows\
Copy-Item .\README.md .\dist\windows\

# Create ZIP
Compress-Archive -Path .\dist\windows\* -DestinationPath .\timecoin-windows-x64.zip
```

---

## Summary

✅ **Windows is fully supported!**

The TIME Coin daemon is designed to be cross-platform from the ground up. All dependencies are Windows-compatible, and there are no Unix-specific system calls.

Key points:
- Works on Windows 10/11 and Windows Server
- No code changes needed for Windows
- Standard Rust toolchain is all that's required
- Can run as a Windows Service
- Performance is comparable to Linux

For support, see the main README or join our community channels.
