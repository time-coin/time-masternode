# Building TIME Coin on Windows

This guide provides step-by-step instructions for building TIME Coin on Windows.

## Prerequisites

1. **Install Rust**
   - Download and run [rustup-init.exe](https://rustup.rs/)
   - Follow the installation prompts
   - Restart your terminal after installation

2. **Install Visual Studio Build Tools**
   - Download [Visual Studio Build Tools](https://visualstudio.microsoft.com/downloads/#build-tools-for-visual-studio-2022)
   - Run the installer and select "Desktop development with C++"
   - Make sure to include:
     - MSVC v143 - VS 2022 C++ x64/x86 build tools
     - Windows 10/11 SDK

3. **Install CMake**
   - Download [CMake](https://cmake.org/download/)
   - During installation, select "Add CMake to the system PATH for all users"
   - Or use chocolatey: `choco install cmake`

4. **Install NASM (Netwide Assembler)**
   - Download [NASM](https://www.nasm.us/pub/nasm/releasebuilds/)
   - Extract to `C:\Program Files\NASM\` 
   - Add `C:\Program Files\NASM\` to your PATH
   - Or use chocolatey: `choco install nasm`

## Verify Installation

Open a new PowerShell window and verify:

```powershell
rustc --version
cargo --version
cmake --version
nasm --version
```

## Building

1. Clone the repository:
```powershell
git clone https://github.com/yourusername/timecoin.git
cd timecoin
```

2. Build the project:
```powershell
cargo build --release
```

3. Run the daemon:
```powershell
.\target\release\timed.exe
```

## Troubleshooting

### CMake Not Found
- Make sure CMake is in your PATH
- Restart your terminal after installing
- Try: `$env:Path = [System.Environment]::GetEnvironmentVariable("Path","Machine") + ";" + [System.Environment]::GetEnvironmentVariable("Path","User")`

### NASM Not Found
- Verify NASM is installed and in PATH
- Try: `$env:Path += ";C:\Program Files\NASM"`
- Restart terminal

### Link Errors
- Ensure Visual Studio Build Tools are fully installed
- May need to run in "Developer PowerShell for VS 2022"

## Known Issues

- First build may take 10-15 minutes due to dependency compilation
- Some crates require internet access during build
