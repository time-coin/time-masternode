@echo off
REM TIME Coin Masternode Configuration Script (Windows)
REM 
REM This script helps you configure your masternode settings in config.toml
REM It prompts for all necessary information and updates the configuration file.

setlocal enabledelayedexpansion

REM Determine user's data directory
if defined APPDATA (
    set "TIMECOIN_DIR=%APPDATA%\timecoin"
) else (
    set "TIMECOIN_DIR=%USERPROFILE%\.timecoin"
)

echo.
echo ╔════════════════════════════════════════════════╗
echo ║   TIME Coin Masternode Configuration Tool     ║
echo ╚════════════════════════════════════════════════╝
echo.

REM Check for command-line argument
if not "%~1"=="" (
    set "NETWORK_ARG=%~1"
    call :tolower NETWORK_ARG
    
    if "!NETWORK_ARG!"=="mainnet" (
        set "CONFIG_FILE=%TIMECOIN_DIR%\config.toml"
        set "NETWORK=mainnet"
        goto check_config
    ) else if "!NETWORK_ARG!"=="testnet" (
        set "CONFIG_FILE=%TIMECOIN_DIR%\testnet\config.toml"
        set "NETWORK=testnet"
        goto check_config
    ) else (
        echo [ERROR] Invalid network '%~1'
        echo Usage: %~nx0 [mainnet^|testnet]
        echo.
        echo Examples:
        echo   %~nx0 mainnet    # Configure mainnet
        echo   %~nx0 testnet    # Configure testnet
        echo   %~nx0            # Defaults to mainnet
        pause
        exit /b 1
    )
)

REM Default to mainnet if no argument provided
set "CONFIG_FILE=%TIMECOIN_DIR%\config.toml"
set "NETWORK=mainnet"
echo No network specified, defaulting to mainnet
echo.

:check_config

REM Check if config file exists
if not exist "%CONFIG_FILE%" (
    echo [ERROR] Config file not found: %CONFIG_FILE%
    echo.
    echo Possible reasons:
    echo   1. Node has not been run yet (run 'timed' first to create config^)
    echo   2. Config is in a different location
    echo.
    set /p "custom_path=Would you like to specify a custom config file path? (y/n): "
    if /i "%custom_path%"=="y" (
        set /p "CONFIG_FILE=Enter full path to config.toml: "
        if not exist "!CONFIG_FILE!" (
            echo [ERROR] File not found: !CONFIG_FILE!
            pause
            exit /b 1
        )
    ) else (
        pause
        exit /b 1
    )
)

echo.
echo This script will help you configure your masternode settings.
echo Network: %NETWORK%
echo Configuration file: %CONFIG_FILE%
echo.

REM Backup existing config
set "TIMESTAMP=%date:~-4%%date:~-10,2%%date:~-7,2%_%time:~0,2%%time:~3,2%%time:~6,2%"
set "TIMESTAMP=%TIMESTAMP: =0%"
set "BACKUP_FILE=%CONFIG_FILE%.backup.%TIMESTAMP%"
copy "%CONFIG_FILE%" "%BACKUP_FILE%" >nul
echo [OK] Created backup: %BACKUP_FILE%
echo.

REM Step 1: Enable masternode?
echo Step 1: Enable Masternode
echo Do you want to enable masternode functionality? (y/n)
:ask_enable
set /p "enable_input=> "
if /i "%enable_input%"=="y" (
    set "MASTERNODE_ENABLED=true"
    goto tier_select
) else if /i "%enable_input%"=="yes" (
    set "MASTERNODE_ENABLED=true"
    goto tier_select
) else if /i "%enable_input%"=="n" (
    set "MASTERNODE_ENABLED=false"
    goto disable_mn
) else if /i "%enable_input%"=="no" (
    set "MASTERNODE_ENABLED=false"
    goto disable_mn
) else (
    echo [ERROR] Invalid input. Please enter 'y' or 'n'
    goto ask_enable
)

:disable_mn
echo Masternode will be disabled.
powershell -Command "(Get-Content '%CONFIG_FILE%') -replace '^enabled = .*', 'enabled = false' | Set-Content '%CONFIG_FILE%'"
echo [OK] Configuration updated successfully!
pause
exit /b 0

:tier_select
echo.
echo Step 2: Select Masternode Tier
echo Available tiers:
echo   - Free:   No collateral (basic rewards, no governance voting)
echo   - Bronze: 1,000 TIME collateral (10x rewards, governance voting)
echo   - Silver: 10,000 TIME collateral (100x rewards, governance voting)
echo   - Gold:   100,000 TIME collateral (1000x rewards, governance voting)
echo.
echo Enter tier (free/bronze/silver/gold):
:ask_tier
set /p "tier_input=> "
set "tier_lower=%tier_input%"
call :tolower tier_lower

if "%tier_lower%"=="free" (
    set "TIER=free"
    goto reward_address
) else if "%tier_lower%"=="bronze" (
    set "TIER=bronze"
    goto reward_address
) else if "%tier_lower%"=="silver" (
    set "TIER=silver"
    goto reward_address
) else if "%tier_lower%"=="gold" (
    set "TIER=gold"
    goto reward_address
) else (
    echo [ERROR] Invalid tier. Please enter: free, bronze, silver, or gold
    goto ask_tier
)

:reward_address
echo.
echo Step 3: Reward Address
echo Enter your TIME address where you want to receive rewards:
echo (Must start with 'TIME' - example: TIME1abc...)
:ask_reward_address
set /p "REWARD_ADDRESS=> "
if "%REWARD_ADDRESS%"=="" (
    echo [ERROR] Reward address cannot be empty
    goto ask_reward_address
)
REM Basic validation - check if starts with TIME
echo %REWARD_ADDRESS% | findstr /b "TIME" >nul
if errorlevel 1 (
    echo [WARNING] Address format looks incorrect (should start with TIME)
    echo Continue anyway? (y/n)
    set /p "continue_anyway=> "
    if /i not "%continue_anyway%"=="y" goto ask_reward_address
)

echo.

REM Step 4: Collateral information (only if not free tier)
if "%TIER%"=="free" (
    set "COLLATERAL_TXID="
    set "COLLATERAL_VOUT="
    goto summary
)

echo Step 4: Collateral Information
echo.
echo To lock collateral, you need to provide the UTXO details:
echo   1. Run: time-cli listunspent
echo   2. Find the UTXO with your collateral amount
echo   3. Note the txid and vout
echo.

REM Get collateral txid
echo Enter collateral transaction ID (txid):
echo (64 hex characters - example: abc123def456...)
:ask_collateral_txid
set /p "COLLATERAL_TXID=> "
if "%COLLATERAL_TXID%"=="" (
    echo [INFO] You can leave this empty and configure later
    echo Continue without collateral txid? (y/n)
    set /p "skip_collateral=> "
    if /i "%skip_collateral%"=="y" (
        set "COLLATERAL_VOUT="
        goto summary
    )
    goto ask_collateral_txid
)

REM Validate txid length (should be 64 chars)
call :strlen len COLLATERAL_TXID
if not !len!==64 (
    echo [ERROR] Invalid txid format (must be 64 hex characters)
    goto ask_collateral_txid
)

echo.
echo Enter collateral output index (vout):
echo (Usually 0 or 1 - check listunspent output)
:ask_collateral_vout
set /p "COLLATERAL_VOUT=> "
REM Validate vout is numeric
echo %COLLATERAL_VOUT% | findstr /r "^[0-9][0-9]*$" >nul
if errorlevel 1 (
    echo [ERROR] Invalid vout (must be a non-negative integer)
    goto ask_collateral_vout
)

:summary
echo.
echo ════════════════════════════════════════════════
echo Configuration Summary
echo ════════════════════════════════════════════════
echo Masternode Enabled:  %MASTERNODE_ENABLED%
echo Tier:                %TIER%
echo Reward Address:      %REWARD_ADDRESS%
if not "%COLLATERAL_TXID%"=="" (
    echo Collateral TXID:     %COLLATERAL_TXID%
    echo Collateral VOUT:     %COLLATERAL_VOUT%
) else (
    echo Collateral:          Not configured (can register later via CLI^)
)
echo ════════════════════════════════════════════════
echo.
echo Save this configuration? (y/n)
set /p "confirm=> "
if /i not "%confirm%"=="y" (
    echo [CANCELLED] Configuration cancelled
    echo Backup preserved at: %BACKUP_FILE%
    pause
    exit /b 1
)

REM Update config.toml using PowerShell
echo.
echo Updating config.toml...

REM Update enabled
powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; $content -replace '(?m)(?<=\[masternode\].*?)^enabled = .*$', 'enabled = %MASTERNODE_ENABLED%' | Set-Content '%CONFIG_FILE%'"

REM Update tier
powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; $content -replace '(?m)(?<=\[masternode\].*?)^tier = .*$', 'tier = \"%TIER%\"' | Set-Content '%CONFIG_FILE%'"

REM Add or update reward_address
powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; if ($content -match 'reward_address =') { $content -replace '(?m)(?<=\[masternode\].*?)^reward_address = .*$', 'reward_address = \"%REWARD_ADDRESS%\"' } else { $content -replace '(?m)(tier = .*)', \"$1`nreward_address = \"\"%REWARD_ADDRESS%\"\"\" } | Set-Content '%CONFIG_FILE%'"

REM Update collateral_txid
if not "%COLLATERAL_TXID%"=="" (
    powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; $content -replace '(?m)(?<=\[masternode\].*?)^collateral_txid = .*$', 'collateral_txid = \"%COLLATERAL_TXID%\"' | Set-Content '%CONFIG_FILE%'"
) else (
    powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; $content -replace '(?m)(?<=\[masternode\].*?)^collateral_txid = .*$', 'collateral_txid = \"\"' | Set-Content '%CONFIG_FILE%'"
)

REM Add or update collateral_vout
if not "%COLLATERAL_VOUT%"=="" (
    powershell -Command "$content = Get-Content '%CONFIG_FILE%' -Raw; if ($content -match 'collateral_vout =') { $content -replace '(?m)(?<=\[masternode\].*?)^collateral_vout = .*$', 'collateral_vout = %COLLATERAL_VOUT%' } else { $content -replace '(?m)(collateral_txid = .*)', \"$1`ncollateral_vout = %COLLATERAL_VOUT%\" } | Set-Content '%CONFIG_FILE%'"
)

echo [OK] Configuration saved successfully!
echo.
echo Next Steps:
echo.

if "%TIER%"=="free" (
    echo 1. Restart your node to apply changes
    echo    .\target\release\timed.exe
    echo.
    echo 2. Check masternode status
    echo    time-cli masternodestatus
) else (
    if "%COLLATERAL_TXID%"=="" (
        echo 1. Create collateral UTXO:
        echo    time-cli sendtoaddress %REWARD_ADDRESS% ^<amount^>
        echo.
        if "%TIER%"=="bronze" echo    Required amount: 1000.0 TIME
        if "%TIER%"=="silver" echo    Required amount: 10000.0 TIME
        if "%TIER%"=="gold" echo    Required amount: 100000.0 TIME
        echo.
        echo 2. Wait for 3 confirmations (~30 minutes^)
        echo    time-cli listunspent
        echo.
        echo 3. Register masternode with collateral:
        echo    time-cli masternoderegister \
        echo      --tier %TIER% \
        echo      --collateral-txid ^<txid^> \
        echo      --vout ^<vout^> \
        echo      --reward-address %REWARD_ADDRESS%
        echo.
        echo 4. Verify registration:
        echo    time-cli masternodelist
        echo    time-cli listlockedcollaterals
    ) else (
        echo 1. Restart your node to apply changes
        echo    .\target\release\timed.exe
        echo.
        echo 2. Register masternode with collateral:
        echo    time-cli masternoderegister \
        echo      --tier %TIER% \
        echo      --collateral-txid %COLLATERAL_TXID% \
        echo      --vout %COLLATERAL_VOUT% \
        echo      --reward-address %REWARD_ADDRESS%
        echo.
        echo 3. Verify registration:
        echo    time-cli masternodelist
        echo    time-cli listlockedcollaterals
    )
)

echo.
echo [SUCCESS] Configuration complete!
echo Backup saved at: %BACKUP_FILE%
pause
exit /b 0

REM Helper function to convert to lowercase
:tolower
for %%L IN (a b c d e f g h i j k l m n o p q r s t u v w x y z) DO (
    call set "%~1=%%%~1:%%L=%%L%%"
)
goto :eof

REM Helper function to get string length
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
