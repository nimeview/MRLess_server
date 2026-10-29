#!/bin/sh
# Build libmrless.so for Android ABIs. Requires ANDROID_NDK_HOME.
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  if [ -n "${ANDROID_HOME:-}" ]; then
    ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME/ndk/"* 2>/dev/null | sort -V | tail -1 || true)"
  fi
  if [ -z "${ANDROID_NDK_HOME:-}" ] && [ -d "$HOME/Library/Android/sdk/ndk" ]; then
    ANDROID_NDK_HOME="$(ls -d "$HOME/Library/Android/sdk/ndk/"* 2>/dev/null | sort -V | tail -1 || true)"
  fi
fi
if [ -z "${ANDROID_NDK_HOME:-}" ] || [ ! -d "$ANDROID_NDK_HOME" ]; then
  echo "Set ANDROID_NDK_HOME (or install NDK via Android Studio)."
  exit 1
fi

HOST_TAG=darwin-x86_64
case "$(uname -s)" in
  Linux) HOST_TAG=linux-x86_64 ;;
  Darwin)
    if [ "$(uname -m)" = "arm64" ]; then HOST_TAG=darwin-arm64; fi
    ;;
esac
TOOLCHAIN="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/$HOST_TAG/bin"
if [ ! -d "$TOOLCHAIN" ]; then
  # Fallback common layout
  TOOLCHAIN="$(ls -d "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/"* 2>/dev/null | head -1)/bin"
fi

API=24
export PATH="$TOOLCHAIN:$PATH"

build_abi() {
  target="$1"
  abi="$2"
  cc="$3"
  echo "→ $abi ($target)"
  rustup target add "$target" >/dev/null 2>&1 || true
  export CC="$cc"
  export AR=llvm-ar
  export CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER="$cc"
  export CARGO_TARGET_ARMV7_LINUX_ANDROIDEABI_LINKER="$cc"
  export CARGO_TARGET_X86_64_LINUX_ANDROID_LINKER="$cc"
  cargo build --release --lib --target "$target"
  mkdir -p "android/app/src/main/jniLibs/$abi"
  cp "target/$target/release/libmrless.so" "android/app/src/main/jniLibs/$abi/"
}

build_abi aarch64-linux-android arm64-v8a "aarch64-linux-android${API}-clang"
build_abi armv7-linux-androideabi armeabi-v7a "armv7a-linux-androideabi${API}-clang"
build_abi x86_64-linux-android x86_64 "x86_64-linux-android${API}-clang"

echo "wrote android/app/src/main/jniLibs/*/libmrless.so"
