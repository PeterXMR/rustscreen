#!/usr/bin/env bash
#
# build_release_apk.sh — build the release Android client end-to-end:
#   1. cross-compile the Rust cdylib into jniLibs via cargo-ndk (release profile)
#   2. assemble the release APK via Gradle (signed per android/app/build.gradle.kts)
#
# P8 packaging scaffold. Mirrors the debug flow in the README but uses the release
# profile and the release signing config. If no release keystore is configured the
# Gradle step falls back to the debug key with a warning (see build.gradle.kts) — fine
# for a smoke test, NOT for distribution.
#
# Requirements (same as the debug Android build, see CONTRIBUTING.md / README.md):
#   - rustup target add aarch64-linux-android
#   - cargo install cargo-ndk
#   - Android NDK r25 (25.2.9519653) + a JDK in 17–21 for Gradle
#   - ANDROID_HOME / ANDROID_NDK_HOME exported
#
# For a DISTRIBUTABLE (release-signed) APK, also provide the signing config — either:
#   export RUSTSCREEN_KEYSTORE=/abs/path/release.keystore
#   export RUSTSCREEN_KEYSTORE_PASSWORD=... RUSTSCREEN_KEY_ALIAS=... RUSTSCREEN_KEY_PASSWORD=...
# or copy packaging/android/keystore.properties.example → android/keystore.properties.
#
# Usage:
#   packaging/android/build_release_apk.sh
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"

: "${ANDROID_NDK_HOME:?export ANDROID_NDK_HOME (e.g. \$ANDROID_HOME/ndk/25.2.9519653)}"

echo "==> 1/2 Building android-client .so (release) via cargo-ndk"
# `--features live-decode` compiles the AMediaCodec decode-to-surface adapter and the
# nativeOnSurface/nativeOnSurfaceDestroyed JNI entries the Kotlin shell calls. Omitting it
# ships a .so missing those symbols → UnsatisfiedLinkError crash on launch.
cargo ndk \
    -t arm64-v8a \
    -o "$REPO_ROOT/android/app/src/main/jniLibs" \
    build --release -p android-client --features live-decode

echo "==> 2/2 Assembling release APK via Gradle"
( cd "$REPO_ROOT/android" && ./gradlew assembleRelease )

APK="$REPO_ROOT/android/app/build/outputs/apk/release/app-release.apk"
echo "==> Done."
if [[ -f "$APK" ]]; then
    echo "    APK: $APK"
else
    echo "    Expected APK at: $APK"
    echo "    (Gradle may name it app-release-unsigned.apk if signing is not configured.)"
fi
echo "    Install on a Pixel 6a in developer mode:  adb install \"$APK\""
