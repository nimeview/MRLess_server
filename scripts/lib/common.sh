#!/bin/sh
# Shared helpers for MRLess build/deploy scripts.
# shellcheck disable=SC2034

# Always resolve from this file (scripts/lib/common.sh), even when sourced.
if [ -n "${BASH_SOURCE[0]:-}" ]; then
  _MRLESS_LIB="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
elif [ -n "${ZSH_VERSION:-}" ]; then
  # shellcheck disable=SC2296
  _MRLESS_LIB="$(cd "$(dirname "${(%):-%x}")" && pwd)"
else
  # POSIX: this file path is unknown when sourced — assume scripts/*.sh caller.
  _MRLESS_LIB="$(cd "$(dirname "$0")/lib" && pwd)"
fi
ROOT="$(cd "$_MRLESS_LIB/../.." && pwd)"
BUILDS="$ROOT/builds"
unset _MRLESS_LIB
TEAM_ID="${DEVELOPMENT_TEAM:-}"
STAMP="$(date +%Y-%m-%d_%H%M%S)"

die() {
  echo "error: $*" >&2
  exit 1
}

need() {
  command -v "$1" >/dev/null 2>&1 || die "missing tool: $1"
}

publish_build() {
  # publish_build <platform> <src_app_path>
  platform="$1"
  src="$2"
  [ -e "$src" ] || die "build product missing: $src"

  dest_dir="$BUILDS/$platform/$STAMP"
  latest="$BUILDS/$platform/latest"
  mkdir -p "$dest_dir"
  rm -rf "$dest_dir/$(basename "$src")"
  cp -R "$src" "$dest_dir/"

  mkdir -p "$(dirname "$latest")"
  rm -rf "$latest"
  cp -R "$dest_dir" "$latest"

  # Keep a thin pointer for scripts that expect dist/
  mkdir -p "$ROOT/dist/$platform"
  rm -rf "$ROOT/dist/$platform/$(basename "$src")"
  cp -R "$src" "$ROOT/dist/$platform/"

  echo "published → $dest_dir"
  echo "latest    → $latest"
}

list_ios_devices() {
  if command -v ios-deploy >/dev/null 2>&1; then
    ios-deploy -c -t 3 2>&1 || true
  fi
  xcrun xctrace list devices 2>/dev/null | sed -n '1,40p' || true
}
