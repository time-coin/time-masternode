@echo off
REM TIME Coin Masternode Configuration Script (Windows)
REM
REM Configures time.conf and masternode.conf for masternode operation.
REM Usage: configure-masternode.bat [mainnet|testnet]

setlocal enabledelayedexpansion

REM Determine user's data directory
if defined APPDATA (
    set "BASE_DIR=%APPDATA%\timecoin"
) else (
    set "BASE_DIR=%USERPROFILE%\.timecoin"
)

echo.
echo ╔════════════════════════════════════════════════╗
echo ║   TIME Coin Masternode Configuration Tool     ║
echo ╚════════════════════════════════════════════════╝
echo.

REM Network selection
set "NETWORK=mainnet"
if not "%~1"=="" (
    set "NETWORK_ARG=%~1"
    call :tolower NETWORK_ARG
    if "!NETWORK_ARG!"=="mainnet" (
        set "NETWORK=mainnet"
    ) else if "!NETWORK_ARG!"=="testnet" (
        set "NETWORK=testnet"
    ) else (
        echo [ERROR] Invalid network '%~1'
        echo Usage: %~nx0 [mainnet^|testnet]
        pause
        exit /b 1
    )
)

if "%NETWORK%"=="testnet" (
    set "DATA_DIR=%BASE_DIR%\testnet"
    set "MN_PORT=24100"
) else (
    set "DATA_DIR=%BASE_DIR%"
    set "MN_PORT=24000"
)

set "CONF_FILE=%DATA_DIR%\time.conf"
set "MN_CONF_FILE=%DATA_DIR%\masternode.conf"

echo Network:          %NETWORK%
echo Data directory:   %DATA_DIR%
echo Config file:      %CONF_FILE%
echo Masternode conf:  %MN_CONF_FILE%
echo.

REM Create data directory
if not exist "%DATA_DIR%" mkdir "%DATA_DIR%"

REM Backup existing configs
for /f "tokens=2 delims==" %%a in ('wmic os get localdatetime /value') do set "DT=%%a"
set "TIMESTAMP=!DT:~0,14!"
if exist "%CONF_FILE%" (
    copy "%CONF_FILE%" "%CONF_FILE%.backup.!TIMESTAMP!" >nul
    echo [OK] Backed up time.conf
)
if exist "%MN_CONF_FILE%" (
    copy "%MN_CONF_FILE%" "%MN_CONF_FILE%.backup.!TIMESTAMP!" >nul
    echo [OK] Backed up masternode.conf
)
echo.

REM Step 1: Enable masternode?
echo Step 1: Enable Masternode
echo Do you want to enable masternode functionality? (y/n)
:ask_enable
set /p "enable_input=> "
if /i "%enable_input%"=="y" goto tier_select
if /i "%enable_input%"=="yes" goto tier_select
if /i "%enable_input%"=="n" goto disable_mn
if /i "%enable_input%"=="no" goto disable_mn
echo [ERROR] Invalid input. Please enter 'y' or 'n'
goto ask_enable

:disable_mn
echo Masternode will be disabled.
if not exist "%CONF_FILE%" echo masternode=0> "%CONF_FILE%"
powershell -Command "$c = Get-Content '%CONF_FILE%'; $found=$false; $c = $c | ForEach-Object { if ($_ -match '^masternode=') { $found=$true; 'masternode=0' } else { $_ } }; if (-not $found) { $c += 'masternode=0' }; $c | Set-Content '%CONF_FILE%'"
echo [OK] Set masternode=0 in time.conf
pause
exit /b 0

:tier_select
echo.
echo Step 2: Select Masternode Tier
echo Available tiers:
echo   - Free:   No collateral
echo   - Bronze: 1,000 TIME collateral
echo   - Silver: 10,000 TIME collateral
echo   - Gold:   100,000 TIME collateral
echo.
echo Enter tier (free/bronze/silver/gold):
:ask_tier
set /p "tier_input=> "
set "TIER=%tier_input%"
call :tolower TIER
if "%TIER%"=="free" goto ask_privkey
if "%TIER%"=="bronze" goto ask_privkey
if "%TIER%"=="silver" goto ask_privkey
if "%TIER%"=="gold" goto ask_privkey
echo [ERROR] Invalid tier.
goto ask_tier

:ask_privkey
echo.
echo Step 3: Masternode Private Key
echo Enter your masternode private key
echo (Generate one with: time-cli masternode genkey)
echo Or press Enter to skip (wallet key will be used):
set "MN_PRIVKEY="
set /p "MN_PRIVKEY=> "
echo.

REM Step 4: Collateral (non-free only)
set "COLLATERAL_TXID="
set "COLLATERAL_VOUT="
if "%TIER%"=="free" goto summary

echo Step 4: Collateral Information
echo Enter collateral transaction ID (txid, 64 hex chars):
:ask_txid
set /p "COLLATERAL_TXID=> "
if "%COLLATERAL_TXID%"=="" (
    echo Skip collateral for now? (y/n)
    set /p "skip=> "
    if /i "%skip%"=="y" goto summary
    goto ask_txid
)
call :strlen txid_len COLLATERAL_TXID
if not !txid_len!==64 (
    echo [ERROR] Must be 64 hex characters
    set "COLLATERAL_TXID="
    goto ask_txid
)

echo Enter collateral output index (vout):
:ask_vout
set /p "COLLATERAL_VOUT=> "
echo %COLLATERAL_VOUT% | findstr /r "^[0-9][0-9]*$" >nul
if errorlevel 1 (
    echo [ERROR] Must be a non-negative integer
    goto ask_vout
)

:summary
echo.
echo ════════════════════════════════════════════════
echo Configuration Summary
echo ════════════════════════════════════════════════
echo Network:             %NETWORK%
echo Masternode:          enabled
echo Tier:                %TIER%
if not "%MN_PRIVKEY%"=="" (echo Private Key:         %MN_PRIVKEY:~0,8%...) else (echo Private Key:         [wallet key])
if not "%COLLATERAL_TXID%"=="" (
    echo Collateral TXID:     %COLLATERAL_TXID%
    echo Collateral VOUT:     %COLLATERAL_VOUT%
) else if not "%TIER%"=="free" (
    echo Collateral:          Not configured yet
)
echo ════════════════════════════════════════════════
echo.
echo Save this configuration? (y/n)
set /p "confirm=> "
if /i not "%confirm%"=="y" (
    echo [CANCELLED] Configuration cancelled
    pause
    exit /b 1
)

REM ─── Write time.conf ───────────────────────────────────────
echo.
echo Writing time.conf...

if not exist "%CONF_FILE%" (
    if "%NETWORK%"=="testnet" (set "TN_LINE=testnet=1") else (set "TN_LINE=#testnet=0")
    (
        echo # TIME Coin Configuration File
        echo !TN_LINE!
        echo listen=1
        echo server=1
        echo masternode=1
        echo debug=info
        echo txindex=1
    ) > "%CONF_FILE%"
    echo [OK] Created new time.conf
) else (
    echo [OK] Updating existing time.conf
)

REM Set masternode=1
powershell -Command "$c = Get-Content '%CONF_FILE%'; $found=$false; $c = $c | ForEach-Object { if ($_ -match '^#?masternode=') { $found=$true; 'masternode=1' } else { $_ } }; if (-not $found) { $c += 'masternode=1' }; $c | Set-Content '%CONF_FILE%'"

REM Set masternodeprivkey if provided
if not "%MN_PRIVKEY%"=="" (
    powershell -Command "$c = Get-Content '%CONF_FILE%'; $found=$false; $c = $c | ForEach-Object { if ($_ -match '^#?masternodeprivkey=') { $found=$true; 'masternodeprivkey=%MN_PRIVKEY%' } else { $_ } }; if (-not $found) { $c += 'masternodeprivkey=%MN_PRIVKEY%' }; $c | Set-Content '%CONF_FILE%'"
)

echo [OK] time.conf updated

REM ─── Write masternode.conf ─────────────────────────────────
echo Writing masternode.conf...

if not "%COLLATERAL_TXID%"=="" (
    set "MN_LINE=mn1 0.0.0.0:%MN_PORT% %COLLATERAL_TXID% %COLLATERAL_VOUT%"
    (
        echo # TIME Coin Masternode Configuration
        echo # Format: alias IP:port collateral_txid collateral_vout
        echo !MN_LINE!
    ) > "%MN_CONF_FILE%"
    echo [OK] masternode.conf updated with collateral
) else if not exist "%MN_CONF_FILE%" (
    (
        echo # TIME Coin Masternode Configuration
        echo # Format: alias IP:port collateral_txid collateral_vout
        echo # Example: mn1 1.2.3.4:%MN_PORT% abc123...def456 0
    ) > "%MN_CONF_FILE%"
    echo [OK] Created masternode.conf template
) else (
    echo [OK] masternode.conf unchanged
)

echo.
echo [SUCCESS] Configuration complete!
echo.
echo Next steps: Restart timed and check status with time-cli masternodestatus
pause
exit /b 0

REM Helper: convert variable to lowercase
:tolower
for %%L IN (a b c d e f g h i j k l m n o p q r s t u v w x y z) DO (
    call set "%~1=%%%~1:%%L=%%L%%"
)
goto :eof

REM Helper: get string length
:strlen
setlocal enabledelayedexpansion
set "str=!%~2!"
set "len=0"
:strlen_loop
if not "!str:~%len%,1!"=="" (
    set /a len+=1
    goto strlen_loop
)
endlocal & set "%~1=%len%"
goto :eof
