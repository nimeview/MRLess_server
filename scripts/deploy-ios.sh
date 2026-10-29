#!/bin/sh
# Build (optional) and install MRLess on connected iPhone(s).
#
# Usage:
#   ./scripts/deploy-ios.sh                  # build + install on all USB devices
#   ./scripts/deploy-ios.sh --no-build       # install builds/ios/latest
#   ./scripts/deploy-ios.sh --id <UDID>      # only that device
#   ./scripts/deploy-ios.sh --list           # list devices
#   ./scripts/deploy-ios.sh --skip-rust      # faster rebuild (UI + Swift only)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

DO_BUILD=1
SKIP_RUST=0
DEVICE_ID=""
LIST_ONLY=0

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) DO_BUILD=0 ;;
    --skip-rust) SKIP_RUST=1 ;;
    --list) LIST_ONLY=1 ;;
    --id)
      shift
      [ $# -gt 0 ] || die "--id needs a UDID"
      DEVICE_ID="$1"
      ;;
    -h|--help)
      sed -n '2,12p' "$0"
      exit 0
      ;;
    *) die "unknown arg: $1" ;;
  esac
  shift
done

need ios-deploy

if [ "$LIST_ONLY" = "1" ]; then
  list_ios_devices
  exit 0
fi

if [ "$DO_BUILD" = "1" ]; then
  export SKIP_RUST
  # SKIP_RUST=1 still packs UI unless SKIP_UI=1
  if [ "$SKIP_RUST" = "1" ]; then
    SKIP_RUST=1 "$ROOT/scripts/build.sh" ios
  else
    "$ROOT/scripts/build.sh" ios
  fi
fi

APP="$BUILDS/ios/latest/MRLess.app"
[ -d "$APP" ] || APP="$ROOT/dist/ios/MRLess.app"
[ -d "$APP" ] || die "no iOS app found — run ./scripts/build.sh ios first"

echo "Installing $APP"

if [ -n "$DEVICE_ID" ]; then
  ios-deploy --id "$DEVICE_ID" --bundle "$APP"
else
  # Prefer USB devices from ios-deploy -c
  ids="$(ios-deploy -c -t 5 2>&1 | sed -n 's/.*Found \([0-9A-Fa-f-]\{25,\}\).*/\1/p' | sort -u)"
  if [ -z "$ids" ]; then
    die "no iOS devices found (enable Developer Mode + trust this Mac)"
  fi
  echo "$ids" | while read -r id; do
    [ -n "$id" ] || continue
    echo "→ $id"
    ios-deploy --id "$id" --bundle "$APP" || echo "warn: install failed on $id" >&2
  done
fi

echo "deploy done."
