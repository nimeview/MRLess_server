#!/bin/sh
# Build signed (debug-keystore) release APK into builds/android/ and dist/android/
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$ROOT"

# Prefer rustup over Homebrew rust (Android targets live in rustup).
export PATH="${HOME}/.cargo/bin:${PATH}"

if [ -z "${JAVA_HOME:-}" ]; then
  if [ -x "${HOME}/sdk/jdk-17/Contents/Home/bin/java" ]; then
    export JAVA_HOME="${HOME}/sdk/jdk-17/Contents/Home"
  elif [ -x "${HOME}/sdk/jdk-17/bin/java" ]; then
    export JAVA_HOME="${HOME}/sdk/jdk-17"
  elif /usr/libexec/java_home -v 17 >/dev/null 2>&1; then
    export JAVA_HOME="$(/usr/libexec/java_home -v 17)"
  fi
fi
if [ -z "${JAVA_HOME:-}" ] || [ ! -x "${JAVA_HOME}/bin/java" ]; then
  echo "JAVA_HOME not set. Install JDK 17 (Temurin) and re-run."
  exit 1
fi
export PATH="${JAVA_HOME}/bin:${PATH}"

if [ -z "${ANDROID_HOME:-}" ] && [ -d "$HOME/Library/Android/sdk" ]; then
  export ANDROID_HOME="$HOME/Library/Android/sdk"
fi
if [ -z "${ANDROID_HOME:-}" ]; then
  echo "ANDROID_HOME not set. Install Android SDK / cmdline-tools, then re-run."
  exit 1
fi

if [ -z "${ANDROID_NDK_HOME:-}" ]; then
  ANDROID_NDK_HOME="$(ls -d "$ANDROID_HOME/ndk/"* 2>/dev/null | sort -V | tail -1 || true)"
  export ANDROID_NDK_HOME
fi

printf 'sdk.dir=%s\n' "$ANDROID_HOME" > "$ROOT/android/local.properties"

"$ROOT/scripts/pack-ui.sh"
"$ROOT/android/build-rust.sh"

cd "$ROOT/android"
chmod +x ./gradlew
./gradlew assembleRelease

APK="$(find "$ROOT/android/app/build/outputs/apk" -name '*.apk' 2>/dev/null | head -1 || true)"
[ -n "$APK" ] || { echo "APK not found under android/app/build/outputs"; exit 1; }

STAMP="$(date +%Y-%m-%d_%H%M%S)"
mkdir -p "$ROOT/builds/android/$STAMP" "$ROOT/builds/android/latest" "$ROOT/dist/android"
cp "$APK" "$ROOT/builds/android/$STAMP/MRLess.apk"
cp "$APK" "$ROOT/builds/android/latest/MRLess.apk"
cp "$APK" "$ROOT/dist/android/MRLess.apk"

echo "Android APK → $ROOT/builds/android/latest/MRLess.apk"
echo "also        → $ROOT/dist/android/MRLess.apk"
ls -lh "$ROOT/builds/android/latest/MRLess.apk"
