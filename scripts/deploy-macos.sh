#!/bin/sh
# Build (optional) and launch the macOS app from builds/macos/latest.
#
# Usage:
#   ./scripts/deploy-macos.sh
#   ./scripts/deploy-macos.sh --no-build
#   ./scripts/deploy-macos.sh --skip-rust
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
# shellcheck disable=SC1091
. "$SCRIPT_DIR/lib/common.sh"

DO_BUILD=1
SKIP_RUST=0

while [ $# -gt 0 ]; do
  case "$1" in
    --no-build) DO_BUILD=0 ;;
    --skip-rust) SKIP_RUST=1 ;;
    -h|--help)
      sed -n '2,10p' "$0"
      exit 0
      ;;
    *) die "unknown arg: $1" ;;
  esac
  shift
done

if [ "$DO_BUILD" = "1" ]; then
  if [ "$SKIP_RUST" = "1" ]; then
    SKIP_RUST=1 "$ROOT/scripts/build.sh" macos
  else
    "$ROOT/scripts/build.sh" macos
  fi
fi

APP="$BUILDS/macos/latest/MRLess.app"
[ -d "$APP" ] || APP="$ROOT/dist/macos/MRLess.app"
[ -d "$APP" ] || die "no macOS app — run ./scripts/build.sh macos"

# Free mesh ports if a stale CLI node is holding them.
for p in 7421 7422; do
  pids="$(lsof -tiTCP:$p -sTCP:LISTEN 2>/dev/null || true)"
  if [ -n "$pids" ]; then
    # Only kill standalone mrless, not already-running MRLess.app we are replacing
    echo "$pids" | xargs kill 2>/dev/null || true
  fi
done
pkill -f 'MRLess.app/Contents/MacOS/MRLess' 2>/dev/null || true
sleep 0.4

open "$APP"
echo "opened $APP"
