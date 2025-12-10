@echo off
REM Test script for TIME Coin node (Windows)

echo Testing TIME Coin Node
echo ==========================
echo.

echo 1. Starting daemon...
start /B .\target\release\timed.exe
timeout /t 3 /nobreak >nul
echo    Daemon started
echo.

echo 2. Testing CLI commands...
echo.

echo Get blockchain info:
.\target\release\time-cli.exe get-blockchain-info
echo.

echo Get block count:
.\target\release\time-cli.exe get-block-count
echo.

echo List masternodes:
.\target\release\time-cli.exe masternode-list
echo.

echo Get consensus info:
.\target\release\time-cli.exe get-consensus-info
echo.

echo Get uptime:
.\target\release\time-cli.exe uptime
echo.

echo Get network info:
.\target\release\time-cli.exe get-network-info
echo.

echo 3. Stopping daemon...
taskkill /IM timed.exe /F >nul 2>&1

echo.
echo Tests complete!
pause
