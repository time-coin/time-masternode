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
start /B "" %DAEMON% --conf time.conf > timed.log 2>&1
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
%CLI% getblockchaininfo
echo.

echo 🔗 Block count:
%CLI% getblockcount
echo.

echo 🌐 Network info:
%CLI% getnetworkinfo
echo.

REM Test wallet commands
echo 4️⃣ Testing wallet commands...
echo.

echo 💰 Get balance:
%CLI% getbalance
echo.

echo 📋 List unspent UTXOs:
%CLI% listunspent
echo.

echo 🔍 Validate address:
%CLI% validateaddress TIME0K8wwmqtqkdG34pdjmMqrXX85TFH7bpM3X
echo.

REM Test masternode commands
echo 5️⃣ Testing masternode commands...
echo.

echo 🏛️ Masternode list:
%CLI% masternodelist
echo.

echo 📊 Masternode status:
%CLI% masternodestatus
echo.

echo ⚖️ Consensus info:
%CLI% getconsensusinfo
echo.

REM Test mempool
echo 6️⃣ Testing mempool commands...
echo.

echo 📦 Mempool info:
%CLI% getmempoolinfo
echo.

echo 📋 Raw mempool:
%CLI% getrawmempool
echo.

REM Wait for block production
echo 7️⃣ Waiting 15 seconds for potential block...
timeout /t 15 /nobreak > nul
echo.

echo 🧱 Block count after wait:
%CLI% getblockcount
echo.

echo 🔍 Get block 1:
%CLI% getblock 1
echo.

REM Test UTXO set
echo 9️⃣ Testing UTXO set info...
echo.

echo 📊 UTXO set info:
%CLI% gettxoutsetinfo
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
