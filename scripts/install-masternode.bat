@echo off
REM TIME Coin Masternode Installation Script (Windows)
REM Usage: install-masternode.bat [mainnet|testnet]
REM Default: mainnet

setlocal enabledelayedexpansion

set "NETWORK=%~1"
if "%NETWORK%"=="" set "NETWORK=mainnet"

if "%NETWORK%"=="mainnet" (
    set "P2P_PORT=24000"
    set "RPC_PORT=24001"
    set "SERVICE_NAME=timed"
    set "TESTNET_FLAG="
    set "DATA_DIR=%APPDATA%\timecoin"
) else if "%NETWORK%"=="testnet" (
    set "P2P_PORT=24100"
    set "RPC_PORT=24101"
    set "SERVICE_NAME=timetd"
    set "TESTNET_FLAG=--testnet"
    set "DATA_DIR=%APPDATA%\timecoin\testnet"
) else (
    echo ERROR: Network must be 'mainnet' or 'testnet'
    echo Usage: install-masternode.bat [mainnet^|testnet]
    exit /b 1
)

echo ============================================================
echo   TIME Coin Masternode Installer (Windows)
echo   Network: %NETWORK%
echo   P2P Port: %P2P_PORT%
echo   RPC Port: %RPC_PORT%
echo   Data Dir: %DATA_DIR%
echo ============================================================
echo.

REM ── Step 1: Check prerequisites ──────────────────────────────
echo [1/6] Checking prerequisites...

where git >nul 2>&1
if errorlevel 1 (
    echo ERROR: Git is not installed.
    echo Download from: https://git-scm.com/download/win
    exit /b 1
)
echo   Git ............. OK

REM Check for Rust / cargo
if exist "%USERPROFILE%\.cargo\env.bat" call "%USERPROFILE%\.cargo\env.bat"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

where cargo >nul 2>&1
if errorlevel 1 (
    echo   Rust ............ NOT FOUND
    echo.
    echo   Rust is required. Please install it:
    echo     1. Download rustup-init.exe from https://rustup.rs
    echo     2. Run it and accept the defaults
    echo     3. Close and reopen this terminal
    echo     4. Re-run this script
    echo.
    echo   You also need Visual Studio Build Tools:
    echo     https://visualstudio.microsoft.com/visual-cpp-build-tools/
    echo     Select "Desktop development with C++" workload
    exit /b 1
)
for /f "tokens=2" %%v in ('rustc --version') do set "RUST_VER=%%v"
echo   Rust ............ OK (v%RUST_VER%)

echo   Prerequisites OK.
echo.

REM ── Step 2: Clone or update repository ───────────────────────
echo [2/6] Setting up source code...

if exist "%USERPROFILE%\time-masternode\.git" (
    echo   Repository exists, pulling latest...
    cd /d "%USERPROFILE%\time-masternode"
    git stash >nul 2>&1
    git pull origin main
) else (
    echo   Cloning repository...
    cd /d "%USERPROFILE%"
    git clone https://github.com/time-coin/time-masternode.git
    cd /d "%USERPROFILE%\time-masternode"
)
echo.

REM ── Step 3: Build ────────────────────────────────────────────
echo [3/6] Building release binaries (this may take several minutes)...

cargo build --release --bin timed --bin time-cli
if errorlevel 1 (
    echo ERROR: Build failed. Check the output above.
    exit /b 1
)
echo   Build complete.
echo.

REM ── Step 4: Create data directory and config ─────────────────
echo [4/6] Setting up data directory...

if not exist "%DATA_DIR%" (
    mkdir "%DATA_DIR%"
    echo   Created %DATA_DIR%
)

if not exist "%DATA_DIR%\time.conf" (
    echo   Creating default time.conf...

    REM Generate random RPC credentials
    set "CHARS=abcdefghijklmnopqrstuvwxyz0123456789"
    set "RPC_USER=timerpc"
    set "RPC_PASS="
    for /L %%i in (1,1,24) do (
        set /a "idx=!random! %% 36"
        for %%j in (!idx!) do set "RPC_PASS=!RPC_PASS!!CHARS:~%%j,1!"
    )

    (
        if "%NETWORK%"=="testnet" echo testnet=1
        echo listen=1
        echo rpcbind=127.0.0.1
        echo rpcuser=!RPC_USER!
        echo rpcpassword=!RPC_PASS!
        echo masternode=1
    ) > "%DATA_DIR%\time.conf"

    echo   Config written to %DATA_DIR%\time.conf
    echo.
    echo   IMPORTANT: Edit time.conf to set your reward_address:
    echo     notepad "%DATA_DIR%\time.conf"
) else (
    echo   time.conf already exists, skipping.
)
echo.

REM ── Step 5: Copy binaries ────────────────────────────────────
echo [5/6] Installing binaries...

copy /Y "target\release\timed.exe" "%USERPROFILE%\time-masternode\timed.exe" >nul
copy /Y "target\release\time-cli.exe" "%USERPROFILE%\time-masternode\time-cli.exe" >nul

REM Add to PATH if not already there
echo %PATH% | find /I "time-masternode" >nul
if errorlevel 1 (
    setx PATH "%PATH%;%USERPROFILE%\time-masternode" >nul 2>&1
    set "PATH=%PATH%;%USERPROFILE%\time-masternode"
    echo   Added time-masternode to PATH (restart terminal for effect)
)
echo   Binaries installed.
echo.

REM ── Step 6: Firewall rule ────────────────────────────────────
echo [6/6] Configuring firewall...

netsh advfirewall firewall show rule name="TIME P2P %NETWORK%" >nul 2>&1
if errorlevel 1 (
    netsh advfirewall firewall add rule name="TIME P2P %NETWORK%" dir=in action=allow protocol=tcp localport=%P2P_PORT% >nul 2>&1
    if errorlevel 1 (
        echo   WARNING: Could not add firewall rule. Run as Administrator to fix.
    ) else (
        echo   Firewall rule added for port %P2P_PORT%.
    )
) else (
    echo   Firewall rule already exists.
)
echo.

REM ── Done ─────────────────────────────────────────────────────
echo ============================================================
echo   Installation complete!
echo ============================================================
echo.
echo   To start your node:
echo     cd %USERPROFILE%\time-masternode
if "%NETWORK%"=="testnet" (
    echo     timed.exe --testnet
) else (
    echo     timed.exe
)
echo.
echo   To run as a Windows service (requires NSSM):
echo     nssm install %SERVICE_NAME% "%USERPROFILE%\time-masternode\timed.exe" "%TESTNET_FLAG%"
echo     nssm start %SERVICE_NAME%
echo.
echo   Useful commands:
echo     time-cli getblockchaininfo
echo     time-cli getpeerinfo
echo     time-cli masternodestatus
echo.
echo   Configuration: notepad "%DATA_DIR%\time.conf"
echo   Dashboard:     cargo run --bin time-dashboard --features dashboard
echo.
echo   To update later: scripts\update.bat %NETWORK%
echo.

endlocal
