@echo off
REM TIME Coin Masternode Uninstall Script (Windows)
REM Usage: scripts\uninstall-masternode.bat [mainnet|testnet]
REM Default: mainnet
REM
REM What this removes:
REM   - Running timed/timetd process (taskkill)
REM   - NSSM service (if installed)
REM   - Windows Firewall rule for the P2P port
REM   - Binaries in %USERPROFILE%\time-masternode\
REM
REM What is PRESERVED by default:
REM   - Blockchain data and wallet (%APPDATA%\timecoin\)
REM   - Source code (%USERPROFILE%\time-masternode\)
REM   - You will be prompted to remove data if you want.

setlocal enabledelayedexpansion

set "NETWORK=%~1"
if "%NETWORK%"=="" set "NETWORK=mainnet"

if "%NETWORK%"=="mainnet" (
    set "P2P_PORT=24000"
    set "SERVICE_NAME=timed"
    set "EXE_NAME=timed.exe"
    set "DATA_DIR=%APPDATA%\timecoin"
) else if "%NETWORK%"=="testnet" (
    set "P2P_PORT=24100"
    set "SERVICE_NAME=timetd"
    set "EXE_NAME=timetd.exe"
    set "DATA_DIR=%APPDATA%\timecoin\testnet"
) else (
    echo ERROR: Network must be 'mainnet' or 'testnet'
    echo Usage: scripts\uninstall-masternode.bat [mainnet^|testnet]
    exit /b 1
)

echo ============================================================
echo   TIME Coin Masternode Uninstaller (Windows)
echo   Network: %NETWORK%
echo ============================================================
echo.
echo The following will be removed:
echo   - Process: %EXE_NAME% (if running)
echo   - NSSM service: %SERVICE_NAME% (if installed)
echo   - Firewall rule for port %P2P_PORT%
echo   - Binaries: %USERPROFILE%\time-masternode\%EXE_NAME%
echo              %USERPROFILE%\time-masternode\time-cli.exe
echo.
echo The following will be PRESERVED:
echo   - Blockchain data and wallet: %DATA_DIR%
echo   - Source code: %USERPROFILE%\time-masternode\
echo.

set /p CONFIRM="Type 'yes' to continue: "
if /i not "%CONFIRM%"=="yes" (
    echo Uninstall cancelled.
    exit /b 0
)
echo.

REM ── Step 1: Stop the running process ────────────────────────
echo [1/4] Stopping %SERVICE_NAME%...

REM Try NSSM stop first (graceful)
where nssm >nul 2>&1
if not errorlevel 1 (
    nssm status %SERVICE_NAME% >nul 2>&1
    if not errorlevel 1 (
        echo   Stopping NSSM service %SERVICE_NAME%...
        nssm stop %SERVICE_NAME% >nul 2>&1
        timeout /t 3 /nobreak >nul
    )
)

REM Kill any remaining process
tasklist /FI "IMAGENAME eq %EXE_NAME%" 2>nul | find /I "%EXE_NAME%" >nul
if not errorlevel 1 (
    echo   Killing %EXE_NAME%...
    taskkill /IM "%EXE_NAME%" /F >nul 2>&1
    timeout /t 2 /nobreak >nul
)
echo   Done.
echo.

REM ── Step 2: Remove NSSM service ─────────────────────────────
echo [2/4] Removing NSSM service...

where nssm >nul 2>&1
if errorlevel 1 (
    echo   NSSM not found — skipping.
) else (
    nssm status %SERVICE_NAME% >nul 2>&1
    if errorlevel 1 (
        echo   Service '%SERVICE_NAME%' not found — skipping.
    ) else (
        nssm remove %SERVICE_NAME% confirm >nul 2>&1
        echo   Service '%SERVICE_NAME%' removed.
    )
)
echo.

REM ── Step 3: Remove firewall rule ────────────────────────────
echo [3/4] Removing firewall rule...

netsh advfirewall firewall show rule name="TIME P2P %NETWORK%" >nul 2>&1
if errorlevel 1 (
    echo   Firewall rule not found — skipping.
) else (
    netsh advfirewall firewall delete rule name="TIME P2P %NETWORK%" >nul 2>&1
    echo   Firewall rule for port %P2P_PORT% removed.
)
echo.

REM ── Step 4: Remove binaries ──────────────────────────────────
echo [4/4] Removing binaries...

set "BIN_DIR=%USERPROFILE%\time-masternode"

if exist "%BIN_DIR%\%EXE_NAME%" (
    del /F /Q "%BIN_DIR%\%EXE_NAME%"
    echo   Deleted %BIN_DIR%\%EXE_NAME%
) else (
    echo   %EXE_NAME% not found — skipping.
)

REM Only remove time-cli.exe if no other network's binary exists
REM (both networks share the same time-cli.exe)
if "%NETWORK%"=="mainnet" (
    if not exist "%BIN_DIR%\timetd.exe" (
        if exist "%BIN_DIR%\time-cli.exe" (
            del /F /Q "%BIN_DIR%\time-cli.exe"
            echo   Deleted %BIN_DIR%\time-cli.exe
        )
    ) else (
        echo   Keeping time-cli.exe (testnet still installed).
    )
) else (
    if not exist "%BIN_DIR%\timed.exe" (
        if exist "%BIN_DIR%\time-cli.exe" (
            del /F /Q "%BIN_DIR%\time-cli.exe"
            echo   Deleted %BIN_DIR%\time-cli.exe
        )
    ) else (
        echo   Keeping time-cli.exe (mainnet still installed).
    )
)
echo.

REM ── Done ──────────────────────────────────────────────────────
echo ============================================================
echo   Uninstall complete.
echo ============================================================
echo.
echo   Blockchain data and wallet are preserved at:
echo     %DATA_DIR%
echo.
echo   To permanently delete all data (IRREVERSIBLE - includes wallet!):
echo     rmdir /s /q "%DATA_DIR%"
echo.
echo   To also remove the source code:
echo     rmdir /s /q "%USERPROFILE%\time-masternode"
echo.

endlocal
