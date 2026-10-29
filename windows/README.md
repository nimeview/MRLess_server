# MRLess for Windows

Portable package: `mrless.exe` + `www/` + `MRLess.bat`.

## Build (on Windows)

```powershell
cd ..\MRLess
bun install
bun run build
cd ..\MRLess_server
.\scripts\pack-ui.sh   # or copy dist → windows\www manually
cargo build --release --bin mrless
copy target\release\mrless.exe windows\
copy /Y windows\MRLess.bat dist\windows\
```

Or from Git Bash / WSL: `./scripts/build-windows.sh`

## Run

Double-click `MRLess.bat`. It starts the mesh node and opens the UI in your browser at http://127.0.0.1:3000.

A full WebView2 native shell can replace the browser later; the node + UI are the same.
