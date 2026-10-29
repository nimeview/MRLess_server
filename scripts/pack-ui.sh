#!/bin/sh
# Build Solid UI into dist/ and copy into platform www folders.
set -euo pipefail
SERVER="$(cd "$(dirname "$0")/.." && pwd)"
UI="$(cd "$SERVER/../MRLess" && pwd)"

cd "$UI"
bun install --frozen-lockfile 2>/dev/null || bun install
bun run build

copy_www() {
  dest="$1"
  rm -rf "$dest"
  mkdir -p "$(dirname "$dest")"
  cp -R "$UI/dist" "$dest"
  echo "www → $dest"
}

copy_www "$SERVER/ios/MRLess/www"
copy_www "$SERVER/macos/MRLess/www"
copy_www "$SERVER/android/app/src/main/assets/www"
copy_www "$SERVER/windows/www"

echo "UI packed"
