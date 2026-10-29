@echo off
setlocal
cd /d "%~dp0"
set INSTANCE=win
set MESH=7421
set WS=7422
set UI=3000

start "MRLess node" /min "%~dp0mrless.exe" --instance %INSTANCE% --mesh-port %MESH% --ws-port %WS% --www "%~dp0www" --ui-port %UI%
timeout /t 1 /nobreak >nul
start "" "http://127.0.0.1:%UI%/"
echo MRLess is running. Close the minimized console window to stop the node.
endlocal
