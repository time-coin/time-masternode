# Windows Build Instructions

## Prerequisites

### Required Software
1. **Visual Studio 2019 or later** with C++ build tools
   - Install "Desktop development with C++" workload
   - Includes MSVC compiler and Windows SDK

2. **CMake** (version 3.15 or later)
   - Download from: https://cmake.org/download/
   - Or install via chocolatey: `choco install cmake`
   - Add CMake to PATH during installation

3. **Rust** (latest stable)
   - Download from: https://rustup.rs/
   - Use `rustup-init.exe`

### Optional but Recommended
- **Git for Windows** - https://git-scm.com/download/win
- **LLVM/Clang** - Some dependencies may prefer clang over MSVC

## Build Steps

### 1. Install CMake
```powershell
# Via chocolatey (recommended)
choco install cmake --installargs 'ADD_CMAKE_TO_PATH=System'

# Or download installer from cmake.org
# Make sure to check "Add CMake to system PATH" during installation
```

### 2. Verify CMake Installation
```powershell
cmake --version
# Should output: cmake version 3.x.x or higher
```

### 3. Clone Repository
```powershell
git clone https://github.com/your-org/timecoin.git
cd timecoin
```

### 4. Build Project
```powershell
# Debug build
cargo build

# Release build (recommended)
cargo build --release
```

## Common Issues

### Issue: "CMake not found"
**Solution**: Add CMake to PATH
```powershell
# Temporary fix (current session only)
$env:Path += ";C:\Program Files\CMake\bin"

# Permanent fix: Add to system environment variables
# System Properties > Environment Variables > Path > Add: C:\Program Files\CMake\bin
```

### Issue: "VCPKG or MSBuild not found"
**Solution**: Install Visual Studio C++ build tools
1. Download Visual Studio Community: https://visualstudio.microsoft.com/
2. Run installer and select "Desktop development with C++"
3. Ensure these components are checked:
   - MSVC v142 or later
   - Windows 10 SDK
   - C++ CMake tools for Windows

### Issue: RocksDB compilation fails
**Solution**: RocksDB requires specific environment
```powershell
# Set environment variable before building
$env:ROCKSDB_STATIC = "1"
cargo build --release
```

### Issue: OpenSSL errors
**Solution**: Install OpenSSL or use rustls feature
```powershell
# Option 1: Install OpenSSL for Windows
# Download from: https://slproweb.com/products/Win32OpenSSL.html

# Option 2: Use rustls instead (if available in dependencies)
cargo build --release --no-default-features --features rustls
```

## Performance Notes

- **Release builds** are significantly faster (10-100x) than debug builds
- First build will take 10-30 minutes as dependencies compile
- Subsequent builds are much faster thanks to incremental compilation
- Consider using `--jobs N` to control parallel compilation if memory is limited

## Running the Node

```powershell
# Run in testnet mode (default)
.\target\release\timed.exe

# Run in mainnet mode
.\target\release\timed.exe --config config.mainnet.toml
```

## Troubleshooting

### Enable verbose build output
```powershell
$env:RUST_BACKTRACE = "1"
cargo build --release --verbose
```

### Clean build (if something is corrupted)
```powershell
cargo clean
cargo build --release
```

### Check system requirements
- **RAM**: Minimum 4GB, recommended 8GB+ for compilation
- **Disk**: ~10GB free space for build artifacts
- **CPU**: Multi-core recommended for faster compilation

## Development on Windows

### Recommended IDE Setup
- **VS Code** with rust-analyzer extension
- **Visual Studio** with Rust plugin
- **RustRover** by JetBrains

### Cross-compilation from Windows
```powershell
# Install Linux target
rustup target add x86_64-unknown-linux-gnu

# Install cross-compilation toolchain
cargo install cross

# Build for Linux
cross build --target x86_64-unknown-linux-gnu --release
```

## Additional Resources

- Rust Windows FAQ: https://github.com/rust-lang/rustup/blob/master/doc/user-guide/src/installation/windows.md
- CMake Documentation: https://cmake.org/documentation/
- Visual Studio Build Tools: https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022
