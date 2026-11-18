@echo off
rem Builds the panel into %LOCALAPPDATA%\Programs\AgentManager\agent-manager-gui.exe
rem IMPORTANT: close the running panel before rebuilding.
rem Do not run the exe from a low-integrity directory.
rem Rust toolchain: https://rustup.rs  (cargo must be on PATH)
setlocal
where cargo >nul 2>nul
if errorlevel 1 (
  echo cargo not found - install Rust via https://rustup.rs and reopen the shell
  exit /b 1
)

cd /d "%~dp0"
set "DEST=%LOCALAPPDATA%\Programs\AgentManager"
if not exist "%DEST%" mkdir "%DEST%"

cargo build --release --bin agent-manager-gui
if errorlevel 1 (
  echo BUILD FAILED
  exit /b 1
)

copy /y "target\release\agent-manager-gui.exe" "%DEST%\agent-manager-gui.exe" >nul
if errorlevel 1 (
  echo COPY FAILED
  exit /b 1
)
echo BUILD OK -^> %DEST%\agent-manager-gui.exe
