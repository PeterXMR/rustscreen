#!/usr/bin/env bash
#
# make_app.sh — assemble RustScreen.app from a release build of the host binary.
#
# This produces ONLY the bundle layout (no signing). Signing + notarization is a
# separate step (sign_and_notarize.sh) so an unsigned bundle can be built and
# smoke-tested locally without any Apple Developer credentials.
#
# P8 packaging scaffold. The host binary's live pipeline is not finished yet, so a
# bundle built today runs but does not produce a second screen — this script exists
# so the packaging path is ready the moment the pipeline lands.
#
# Usage:
#   packaging/macos/make_app.sh [--features <cargo features>]
#
#   --features   Extra cargo features to enable for the release build. For a real
#                shippable app you will want the live-* features, e.g.
#                  --features live-capture,live-usb,live-inject
#                (Default: none — builds the cross-platform skeleton binary.)
#
# Output:  dist/RustScreen.app
#
# Layout produced:
#   RustScreen.app/
#     Contents/
#       Info.plist            (from packaging/macos/Info.plist, __VERSION__ filled)
#       MacOS/macos-host      (the release binary)
#       Resources/            (icon etc. — placeholder; add AppIcon.icns later)
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKG_DIR="$REPO_ROOT/packaging/macos"
DIST_DIR="$REPO_ROOT/dist"
APP_NAME="RustScreen"
APP_BUNDLE="$DIST_DIR/$APP_NAME.app"
BIN_NAME="macos-host"

FEATURES=""
while [[ $# -gt 0 ]]; do
    case "$1" in
        --features)
            FEATURES="${2:-}"
            shift 2
            ;;
        -h|--help)
            sed -n '2,28p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "make_app.sh: unknown argument: $1" >&2
            exit 2
            ;;
    esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "make_app.sh: must be run on macOS (got $(uname -s))." >&2
    exit 1
fi

# Pull the version out of the [workspace.metadata.rustscreen] section so it lives in one place.
# Scope the match to that section (awk sets a flag on the header, then prints the first
# `version = "..."` after it) instead of grabbing the first version-like line in the whole file —
# the workspace Cargo.toml has no [package], so the old `head -n1` worked only by luck of ordering
# and would silently pick up an unrelated key added above the metadata block.
VERSION="$(
    awk -F'"' '/^\[workspace\.metadata\.rustscreen\]/{f=1} f && /^version = /{print $2; exit}' \
        "$REPO_ROOT/Cargo.toml"
)"
VERSION="${VERSION:-0.0.0}"

# Build with the `dist` profile (size-optimized, stripped) — that is the shipped
# artifact. The default `release` profile is tuned for speed (dev/benchmark); see the
# `[profile.dist]` rationale in the workspace Cargo.toml.
echo "==> Building $BIN_NAME (dist${FEATURES:+, features: $FEATURES})"
if [[ -n "$FEATURES" ]]; then
    cargo build --profile dist -p "$BIN_NAME" --features "$FEATURES"
else
    cargo build --profile dist -p "$BIN_NAME"
fi

BIN_PATH="$REPO_ROOT/target/dist/$BIN_NAME"
if [[ ! -x "$BIN_PATH" ]]; then
    echo "make_app.sh: expected binary not found at $BIN_PATH" >&2
    exit 1
fi

echo "==> Assembling $APP_BUNDLE"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

# Info.plist with the version substituted in.
sed "s/__VERSION__/$VERSION/g" "$PKG_DIR/Info.plist" > "$APP_BUNDLE/Contents/Info.plist"

# The executable.
cp "$BIN_PATH" "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"
chmod +x "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"

# PkgInfo (optional but conventional).
printf 'APPL????' > "$APP_BUNDLE/Contents/PkgInfo"

# AppIcon: placeholder. Drop an AppIcon.icns into packaging/macos/ to include it.
if [[ -f "$PKG_DIR/AppIcon.icns" ]]; then
    cp "$PKG_DIR/AppIcon.icns" "$APP_BUNDLE/Contents/Resources/AppIcon.icns"
else
    echo "    (no packaging/macos/AppIcon.icns — bundle has no icon yet)"
fi

echo "==> Done: $APP_BUNDLE (version $VERSION)"
echo "    Next: packaging/macos/sign_and_notarize.sh to sign + notarize,"
echo "          then packaging/macos/make_dmg.sh to produce a distributable DMG."
