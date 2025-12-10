@echo off
REM TIME Coin Wallet & Transaction Test Script (Windows)

setlocal

set CLI=.\target\release\time-cli.exe
set DAEMON=.\target\release\timed.exe

echo 🧪 TIME Coin Wallet ^& Transaction Test
echo ========================================
echo.

REM Build first
echo 📦 Building project...
cargo build --release
echo.

REM Start daemon in background
echo 1️⃣ Starting daemon...
start /B "" %DAEMON% --config config.toml > timed.log 2>&1
timeout /t 2 /nobreak > nul
echo    Daemon started
echo.

REM Wait for startup
echo 2️⃣ Waiting 5 seconds for startup...
timeout /t 5 /nobreak > nul
echo.

REM Test basic info
echo 3️⃣ Testing basic commands...
echo.

echo 📊 Blockchain info:
%CLI% get-blockchain-info
echo.

echo 🔗 Block count:
%CLI% get-block-count
echo.

echo 🌐 Network info:
%CLI% get-network-info
echo.

REM Test wallet commands
echo 4️⃣ Testing wallet commands...
echo.

echo 💰 Get balance:
%CLI% get-balance
echo.

echo 📋 List unspent UTXOs:
%CLI% list-unspent
echo.

echo 🔍 Validate address:
%CLI% validate-address TIME0K8wwmqtqkdG34pdjmMqrXX85TFH7bpM3X
echo.

REM Test masternode commands
echo 5️⃣ Testing masternode commands...
echo.

echo 🏛️ Masternode list:
%CLI% masternode-list
echo.

echo 📊 Masternode status:
%CLI% masternode-status
echo.

echo ⚖️ Consensus info:
%CLI% get-consensus-info
echo.

REM Test mempool
echo 6️⃣ Testing mempool commands...
echo.

echo 📦 Mempool info:
%CLI% get-mempool-info
echo.

echo 📋 Raw mempool:
%CLI% get-raw-mempool
echo.

REM Wait for block production
echo 7️⃣ Waiting 15 seconds for potential block...
timeout /t 15 /nobreak > nul
echo.

echo 🧱 Block count after wait:
%CLI% get-block-count
echo.

echo 🔍 Get block 1:
%CLI% get-block 1
echo.

REM Test UTXO set
echo 9️⃣ Testing UTXO set info...
echo.

echo 📊 UTXO set info:
%CLI% get-tx-out-set-info
echo.

REM Uptime
echo 🔟 Testing uptime...
echo.

echo ⏱️ Daemon uptime:
%CLI% uptime
echo.

REM Stop daemon
echo 🛑 Stopping daemon...
%CLI% stop
timeout /t 2 /nobreak > nul

echo.
echo ✅ Tests complete!
echo.
echo 💡 To view daemon logs: type timed.log

endlocal
