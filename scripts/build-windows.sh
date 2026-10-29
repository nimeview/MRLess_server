#!/bin/sh
# Build a portable Windows package (run on Windows, or with a Windows Rust target).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

"$ROOT/scripts/pack-ui.sh"

TARGET="${MRLESS_WINDOWS_TARGET:-x86_64-pc-windows-msvc}"
OUT="$ROOT/dist/windows"
rm -rf "$OUT"
mkdir -p "$OUT"

if ! rustup target list --installed | grep -qx "$TARGET"; then
  echo "Installing Rust target $TARGET…"
  rustup target add "$TARGET"
fi

echo "building mrless.exe ($TARGET)"
cargo build --release --bin mrless --target "$TARGET"

BIN="target/$TARGET/release/mrless.exe"
if [ ! -f "$BIN" ]; then
  echo "Binary missing: $BIN"
  echo "On macOS you need a Windows linker (e.g. cargo-xwin) or build on a Windows machine."
  exit 1
fi

cp "$BIN" "$OUT/"
cp -R "$ROOT/windows/www" "$OUT/www"
cp "$ROOT/windows/MRLess.bat" "$OUT/"
cp "$ROOT/windows/README.md" "$OUT/" 2>/dev/null || true

echo "Windows package → $OUT/"
echo "Run MRLess.bat on Windows (starts node + opens browser UI)."
