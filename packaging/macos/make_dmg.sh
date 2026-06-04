#!/usr/bin/env bash
#
# make_dmg.sh — wrap RustScreen.app in a distributable DMG with an /Applications
# drag-install symlink.
#
# P8 packaging scaffold. Uses only `hdiutil` (ships with macOS) so there is no
# third-party dependency. For a fancier window background/layout you can later swap
# in `create-dmg`, but the plain hdiutil path is enough to ship.
#
# Run AFTER make_app.sh (and, for distribution, sign_and_notarize.sh — the DMG
# inherits the app's notarization via the stapled ticket; the DMG itself does not
# need separate notarization for Gatekeeper to pass on the stapled .app inside it,
# though you MAY also notarize the DMG for belt-and-suspenders).
#
# Usage:
#   packaging/macos/make_dmg.sh [path/to/RustScreen.app]
#
# Output:  dist/RustScreen-<version>.dmg
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST_DIR="$REPO_ROOT/dist"
APP_BUNDLE="${1:-$DIST_DIR/RustScreen.app}"
APP_NAME="RustScreen"
VOL_NAME="RustScreen"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "make_dmg.sh: must be run on macOS." >&2
    exit 1
fi
if [[ ! -d "$APP_BUNDLE" ]]; then
    echo "make_dmg.sh: app bundle not found: $APP_BUNDLE (run make_app.sh first)." >&2
    exit 1
fi

VERSION="$(
    /usr/libexec/PlistBuddy -c 'Print :CFBundleShortVersionString' \
        "$APP_BUNDLE/Contents/Info.plist" 2>/dev/null || echo "0.0.0"
)"
DMG_PATH="$DIST_DIR/$APP_NAME-$VERSION.dmg"

echo "==> Staging DMG contents"
STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
cp -R "$APP_BUNDLE" "$STAGE/$APP_NAME.app"
ln -s /Applications "$STAGE/Applications"

echo "==> Building $DMG_PATH"
rm -f "$DMG_PATH"
hdiutil create \
    -volname "$VOL_NAME" \
    -srcfolder "$STAGE" \
    -ov \
    -format UDZO \
    "$DMG_PATH"

echo "==> Done: $DMG_PATH (version $VERSION)"
echo "    Drag RustScreen.app to Applications to install."
