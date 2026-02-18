@echo off
REM deploy-config.bat — Copy the appropriate config template to the runtime data directory
REM
REM Usage:
REM   scripts\deploy-config.bat testnet    — Deploy testnet config
REM   scripts\deploy-config.bat mainnet    — Deploy mainnet config
REM   scripts\deploy-config.bat            — Defaults to testnet

setlocal enabledelayedexpansion

set "REPO_ROOT=%~dp0.."
set "NETWORK=%~1"
if "%NETWORK%"=="" set "NETWORK=testnet"

set "BASE_DIR=%APPDATA%\timecoin"

if /I "%NETWORK%"=="testnet" (
    set "SOURCE=%REPO_ROOT%\config.testnet.toml"
    set "DEST_DIR=%BASE_DIR%\testnet"
) else if /I "%NETWORK%"=="mainnet" (
    set "SOURCE=%REPO_ROOT%\config.mainnet.toml"
    set "DEST_DIR=%BASE_DIR%"
) else (
    echo ❌ Unknown network: %NETWORK%
    echo Usage: %~nx0 [testnet^|mainnet]
    exit /b 1
)

set "DEST=%DEST_DIR%\config.toml"

if not exist "%SOURCE%" (
    echo ❌ Source config not found: %SOURCE%
    exit /b 1
)

REM Create destination directory
if not exist "%DEST_DIR%" mkdir "%DEST_DIR%"

REM Back up existing config if present
if exist "%DEST%" (
    for /f "tokens=2 delims==" %%a in ('wmic os get localdatetime /value') do set "DT=%%a"
    set "TIMESTAMP=!DT:~0,14!"
    copy "%DEST%" "%DEST%.bak.!TIMESTAMP!" >nul
    echo 📋 Backed up existing config to: %DEST%.bak.!TIMESTAMP!
)

REM Copy config
copy "%SOURCE%" "%DEST%" >nul
echo ✅ Deployed %NETWORK% config to: %DEST%

endlocal
