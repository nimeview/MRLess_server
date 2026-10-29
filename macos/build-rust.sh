#!/bin/sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if command -v rustup >/dev/null 2>&1; then
  rustup target add aarch64-apple-darwin x86_64-apple-darwin >/dev/null 2>&1 || true
  export RUSTC="$(rustup which rustc)"
  export CARGO="$(rustup which cargo)"
  cargo() { "$CARGO" "$@"; }
fi

echo "building libmrless for macOS (arm64 + x86_64)"
cargo build --release --lib --target aarch64-apple-darwin
cargo build --release --lib --target x86_64-apple-darwin

rm -rf macos/lib/mrless.xcframework macos/lib/headers macos/lib/libmrless.a
mkdir -p macos/lib/headers
cp include/mrless.h macos/lib/headers/

lipo -create \
  target/aarch64-apple-darwin/release/libmrless.a \
  target/x86_64-apple-darwin/release/libmrless.a \
  -output macos/lib/libmrless.a

if command -v xcodebuild >/dev/null 2>&1 && xcodebuild -version >/dev/null 2>&1; then
  xcodebuild -create-xcframework \
    -library macos/lib/libmrless.a \
    -headers macos/lib/headers \
    -output macos/lib/mrless.xcframework
else
  mkdir -p macos/lib/mrless.xcframework/macos-arm64_x86_64/Headers
  cp include/mrless.h macos/lib/mrless.xcframework/macos-arm64_x86_64/Headers/
  cp macos/lib/libmrless.a macos/lib/mrless.xcframework/macos-arm64_x86_64/
fi

echo "wrote macos/lib/mrless.xcframework"
