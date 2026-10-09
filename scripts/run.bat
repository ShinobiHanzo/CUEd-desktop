@echo off
rem Build (if needed) and start the CUEd desktop daemon with the UI open.
cd /d "%~dp0\.."
cargo build --release -p cuedd || exit /b 1
target\release\cuedd.exe --open %*
