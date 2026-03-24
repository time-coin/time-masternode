@echo off
REM Usage: update.bat [mainnet|testnet|both]
REM Default: both

setlocal enabledelayedexpansion

set "NETWORK=%~1"
if "%NETWORK%"=="" set "NETWORK=both"

REM Ensure cargo is in PATH
if exist "%USERPROFILE%\.cargo\env.bat" call "%USERPROFILE%\.cargo\env.bat"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

echo ============================================
echo   TIME Coin Node Updater (Windows)
echo   Network: %NETWORK%
echo ============================================
echo.

REM Pull latest code
cd /d "%USERPROFILE%\time-masternode"
if errorlevel 1 (
    echo ERROR: Could not find %USERPROFILE%\time-masternode
    echo Clone the repo first: git clone https://github.com/time-coin/time-masternode.git
    exit /b 1
)

git stash
git pull origin main
git log -1
echo.

REM Build
echo Building release binaries...
cargo build --release --bin timed --bin time-cli
if errorlevel 1 (
    echo ERROR: Build failed
    exit /b 1
)
echo Build complete.
echo.

REM Determine data directory
set "BASE_DIR=%APPDATA%\timecoin"

REM Stop, copy, restart for each network
if "%NETWORK%"=="testnet" goto :do_testnet
if "%NETWORK%"=="mainnet" goto :do_mainnet

REM both
call :update_network mainnet timed
call :update_network testnet timetd
goto :done

:do_mainnet
call :update_network mainnet timed
goto :done

:do_testnet
call :update_network testnet timetd
goto :done

:update_network
set "NET=%~1"
set "SVC=%~2"

echo ==^> Updating %NET% (%SVC%)...

REM Stop the running node (if any)
tasklist /FI "IMAGENAME eq %SVC%.exe" 2>NUL | find /I "%SVC%.exe" >NUL
if not errorlevel 1 (
    echo Stopping %SVC%.exe...
    taskkill /IM "%SVC%.exe" /F >NUL 2>&1
    timeout /t 3 /nobreak >NUL
)

REM Copy new binaries
copy /Y "target\release\timed.exe" "%USERPROFILE%\time-masternode\%SVC%.exe" >NUL 2>&1
copy /Y "target\release\time-cli.exe" "%USERPROFILE%\time-masternode\time-cli.exe" >NUL 2>&1

echo %NET% binaries updated.

REM Restart
echo Starting %SVC%...
if "%NET%"=="testnet" (
    start "" "%USERPROFILE%\time-masternode\%SVC%.exe" --testnet
) else (
    start "" "%USERPROFILE%\time-masternode\%SVC%.exe"
)

echo %NET% started.
echo.
goto :eof

:done
echo ============================================
echo   Update complete!
echo ============================================
echo.
echo Check status:  time-cli getblockchaininfo
echo View peers:    time-cli getpeerinfo
echo.
endlocal
