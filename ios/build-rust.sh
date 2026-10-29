#!/bin/sh
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"
export IPHONEOS_DEPLOYMENT_TARGET="${IPHONEOS_DEPLOYMENT_TARGET:-16.0}"

# Homebrew rust has no iOS std; rustup cargo still picks PATH rustc unless set.
if command -v rustup >/dev/null 2>&1; then
  rustup target add aarch64-apple-ios aarch64-apple-ios-sim >/dev/null
  export RUSTC="$(rustup which rustc)"
  export CARGO="$(rustup which cargo)"
  cargo() { "$CARGO" "$@"; }
fi

echo "building libmrless for iPhone OS"
cargo build --release --lib --target aarch64-apple-ios

echo "building libmrless for iOS Simulator"
cargo build --release --lib --target aarch64-apple-ios-sim

rm -rf ios/lib/mrless.xcframework
mkdir -p ios/lib/headers \
  ios/lib/mrless.xcframework/ios-arm64/Headers \
  ios/lib/mrless.xcframework/ios-arm64-simulator/Headers
cp include/mrless.h ios/lib/headers/
cp include/mrless.h ios/lib/mrless.xcframework/ios-arm64/Headers/
cp include/mrless.h ios/lib/mrless.xcframework/ios-arm64-simulator/Headers/
cp target/aarch64-apple-ios/release/libmrless.a ios/lib/mrless.xcframework/ios-arm64/
cp target/aarch64-apple-ios-sim/release/libmrless.a ios/lib/mrless.xcframework/ios-arm64-simulator/
cp "$(dirname "$0")/xcframework-Info.plist" ios/lib/mrless.xcframework/Info.plist

if command -v xcodebuild >/dev/null 2>&1 && xcodebuild -version >/dev/null 2>&1; then
  rm -rf ios/lib/mrless.xcframework
  xcodebuild -create-xcframework \
    -library target/aarch64-apple-ios/release/libmrless.a \
    -headers ios/lib/headers \
    -library target/aarch64-apple-ios-sim/release/libmrless.a \
    -headers ios/lib/headers \
    -output ios/lib/mrless.xcframework
fi

echo "wrote ios/lib/mrless.xcframework"
