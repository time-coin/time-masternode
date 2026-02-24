@echo off
REM deploy-config.bat — Create default time.conf and masternode.conf in the data directory
REM
REM Usage:
REM   scripts\deploy-config.bat testnet    — Deploy testnet config
REM   scripts\deploy-config.bat mainnet    — Deploy mainnet config
REM   scripts\deploy-config.bat            — Defaults to mainnet

setlocal enabledelayedexpansion

set "NETWORK=%~1"
if "%NETWORK%"=="" set "NETWORK=mainnet"

set "BASE_DIR=%APPDATA%\timecoin"

if /I "%NETWORK%"=="testnet" (
    set "DEST_DIR=%BASE_DIR%\testnet"
    set "TN_LINE=testnet=1"
    set "PORT=24100"
) else if /I "%NETWORK%"=="mainnet" (
    set "DEST_DIR=%BASE_DIR%"
    set "TN_LINE=#testnet=0"
    set "PORT=24000"
) else (
    echo ❌ Unknown network: %NETWORK%
    echo Usage: %~nx0 [testnet^|mainnet]
    exit /b 1
)

set "CONF=%DEST_DIR%\time.conf"
set "MN_CONF=%DEST_DIR%\masternode.conf"

REM Create destination directory
if not exist "%DEST_DIR%" mkdir "%DEST_DIR%"

REM Back up existing configs
for /f "tokens=2 delims==" %%a in ('wmic os get localdatetime /value') do set "DT=%%a"
set "TIMESTAMP=!DT:~0,14!"
if exist "%CONF%" (
    copy "%CONF%" "%CONF%.bak.!TIMESTAMP!" >nul
    echo 📋 Backed up time.conf
)
if exist "%MN_CONF%" (
    copy "%MN_CONF%" "%MN_CONF%.bak.!TIMESTAMP!" >nul
    echo 📋 Backed up masternode.conf
)

REM Deploy time.conf
if not exist "%CONF%" (
    (
        echo # TIME Coin Configuration File
        echo !TN_LINE!
        echo listen=1
        echo server=1
        echo masternode=1
        echo #masternodeprivkey=
        echo debug=info
        echo txindex=1
    ) > "%CONF%"
    echo ✅ Created %NETWORK% time.conf at: %CONF%
) else (
    echo ℹ️  time.conf already exists: %CONF% ^(preserved^)
)

REM Deploy masternode.conf
if not exist "%MN_CONF%" (
    (
        echo # TIME Coin Masternode Configuration
        echo # Format: alias IP:port collateral_txid collateral_vout
        echo # Example: mn1 1.2.3.4:!PORT! abc123...def456 0
    ) > "%MN_CONF%"
    echo ✅ Created masternode.conf at: %MN_CONF%
) else (
    echo ℹ️  masternode.conf already exists: %MN_CONF% ^(preserved^)
)

endlocal
