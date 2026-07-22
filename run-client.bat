@echo off
setlocal

cd /d "%~dp0"

echo Building the village client...
cargo build -p village_game
if errorlevel 1 exit /b %errorlevel%

echo Starting the village client...
cargo run -p village_game
exit /b %errorlevel%
