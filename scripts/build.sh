#!/bin/sh
# Build MRLess apps into builds/<platform>/<timestamp>/ and builds/<platform>/latest/
#
# Usage:
#   ./scripts/build.sh ios
#   ./scripts/build.sh macos
#   ./scripts/build.sh ios macos
#   ./scripts/build.sh all
#   SKIP_UI=1 ./scripts/build.sh ios     # reuse last packed www
#   SKIP_RUST=1 ./scripts/build.sh macos # reuse last libmrless
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

usage() {
  cat <<'EOF'
Usage: ./scripts/build.sh <platform…|all>

Platforms: ios  macos  android  windows  all

Env:
  DEVELOPMENT_TEAM   Apple Development Team ID for signing
  SKIP_UI=1          skip Solid UI pack
  SKIP_RUST=1        skip Rust/xcframework rebuild
EOF
}

[ $# -ge 1 ] || { usage; exit 1; }

platforms="$*"
if [ "$platforms" = "all" ]; then
  platforms="ios macos"
fi

need xcodebuild
need bun

if [ "${SKIP_UI:-0}" != "1" ]; then
  "$ROOT/scripts/pack-ui.sh"
fi

build_ios() {
  if [ "${SKIP_RUST:-0}" != "1" ]; then
    "$ROOT/ios/build-rust.sh"
  fi
  xcodebuild \
    -project "$ROOT/ios/MRLess.xcodeproj" \
    -scheme MRLess \
    -destination 'generic/platform=iOS' \
    -configuration Release \
    -derivedDataPath "$ROOT/ios/build" \
    CODE_SIGN_IDENTITY="Apple Development" \
    DEVELOPMENT_TEAM="$TEAM_ID" \
    build
  publish_build ios "$ROOT/ios/build/Build/Products/Release-iphoneos/MRLess.app"
}

build_macos() {
  if [ "${SKIP_RUST:-0}" != "1" ]; then
    "$ROOT/macos/build-rust.sh"
  fi
  xcodebuild \
    -project "$ROOT/macos/MRLess.xcodeproj" \
    -scheme MRLess \
    -configuration Release \
    -derivedDataPath "$ROOT/macos/build" \
    CODE_SIGN_IDENTITY="Apple Development" \
    DEVELOPMENT_TEAM="$TEAM_ID" \
    build
  publish_build macos "$ROOT/macos/build/Build/Products/Release/MRLess.app"
}

build_android() {
  [ -x "$ROOT/scripts/build-android.sh" ] || die "build-android.sh missing"
  "$ROOT/scripts/build-android.sh"
  # Prefer APK if produced under android/
  apk="$(find "$ROOT/android" -name '*.apk' 2>/dev/null | head -1 || true)"
  if [ -n "$apk" ]; then
    mkdir -p "$BUILDS/android/$STAMP"
    cp "$apk" "$BUILDS/android/$STAMP/"
    rm -rf "$BUILDS/android/latest"
    mkdir -p "$BUILDS/android/latest"
    cp "$apk" "$BUILDS/android/latest/"
    echo "published → $BUILDS/android/$STAMP"
  else
    echo "android build finished (no apk path auto-detected)"
  fi
}

build_windows() {
  [ -x "$ROOT/scripts/build-windows.sh" ] || die "build-windows.sh missing"
  "$ROOT/scripts/build-windows.sh"
  echo "windows build finished — copy artifacts into builds/windows/ manually if needed"
}

for p in $platforms; do
  case "$p" in
    ios) build_ios ;;
    macos) build_macos ;;
    android) build_android ;;
    windows) build_windows ;;
    *) die "unknown platform: $p" ;;
  esac
done

echo "done."
