@echo off
REM TIME Coin TUI Dashboard (Windows)
REM Usage: scripts\dashboard.bat

setlocal

REM Ensure cargo is in PATH
if exist "%USERPROFILE%\.cargo\env.bat" call "%USERPROFILE%\.cargo\env.bat"
set "PATH=%USERPROFILE%\.cargo\bin;%PATH%"

cd /d "%USERPROFILE%\time-masternode"
if errorlevel 1 (
    echo ERROR: Could not find %USERPROFILE%\time-masternode
    echo Make sure the repository is cloned there.
    exit /b 1
)

cargo run --bin time-dashboard --features dashboard

endlocal
