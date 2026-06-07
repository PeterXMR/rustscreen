#!/usr/bin/env bash
#
# sign_and_notarize.sh — codesign RustScreen.app with the hardened runtime, then
# submit it to Apple's notary service and staple the ticket.
#
# P8 packaging scaffold. This is the documented, repeatable signing path; it is NOT
# yet validated end-to-end because (a) it needs a real "Developer ID Application"
# certificate and (b) the host's private-CGVirtualDisplay path must be exercised on
# a Mac under the hardened runtime to confirm the entitlement set (risk R8). Treat
# the entitlements in packaging/macos/entitlements.plist as a first guess to refine
# empirically — see the comments in that file.
#
# Distribution channel: notarized DMG, NOT the Mac App Store (the private virtual-
# display API bars MAS — roadmap P2 risk). Notarization checks signing + malware,
# not API use, so a correctly-signed binary is expected to notarize (R8 = Low).
#
# Required environment (no secrets are committed — supply these at run time):
#   SIGN_IDENTITY        e.g. "Developer ID Application: Your Name (TEAMID)"
#   NOTARY_PROFILE       a notarytool keychain profile created once with:
#                          xcrun notarytool store-credentials NOTARY_PROFILE \
#                            --apple-id you@example.com \
#                            --team-id TEAMID \
#                            --password <app-specific-password>
#                        (Alternatively set APPLE_ID / TEAM_ID / APP_PASSWORD and
#                         adapt the notarytool call below.)
#
# Usage:
#   SIGN_IDENTITY="Developer ID Application: ... (TEAMID)" \
#   NOTARY_PROFILE="rustscreen-notary" \
#   packaging/macos/sign_and_notarize.sh [path/to/RustScreen.app]
#
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
PKG_DIR="$REPO_ROOT/packaging/macos"
APP_BUNDLE="${1:-$REPO_ROOT/dist/RustScreen.app}"
ENTITLEMENTS="$PKG_DIR/entitlements.plist"

if [[ "$(uname -s)" != "Darwin" ]]; then
    echo "sign_and_notarize.sh: must be run on macOS." >&2
    exit 1
fi
if [[ ! -d "$APP_BUNDLE" ]]; then
    echo "sign_and_notarize.sh: app bundle not found: $APP_BUNDLE" >&2
    echo "  Run packaging/macos/make_app.sh first." >&2
    exit 1
fi
: "${SIGN_IDENTITY:?set SIGN_IDENTITY to your Developer ID Application identity}"
: "${NOTARY_PROFILE:?set NOTARY_PROFILE to a notarytool keychain profile name}"

echo "==> Codesigning (hardened runtime) $APP_BUNDLE"
# --options runtime enables the hardened runtime (required for notarization).
# --timestamp embeds a secure timestamp (required for notarization).
# NOTE: --deep is intentionally NOT used. Apple deprecates --deep for *signing* (it is only
# valid for verification) — nested code must be signed inside-out, individually. For this
# single-binary bundle one invocation is sufficient; when a helper/framework is ever added,
# sign it first (inside-out) and keep this top-level sign last.
codesign --force \
    --options runtime \
    --timestamp \
    --entitlements "$ENTITLEMENTS" \
    --sign "$SIGN_IDENTITY" \
    "$APP_BUNDLE"

echo "==> Verifying signature"
codesign --verify --strict --verbose=2 "$APP_BUNDLE"

echo "==> Zipping for notarization"
ZIP_PATH="$REPO_ROOT/dist/RustScreen-notarize.zip"
rm -f "$ZIP_PATH"
/usr/bin/ditto -c -k --keepParent "$APP_BUNDLE" "$ZIP_PATH"

echo "==> Submitting to Apple notary service (this can take a few minutes)"
xcrun notarytool submit "$ZIP_PATH" \
    --keychain-profile "$NOTARY_PROFILE" \
    --wait

echo "==> Stapling the notarization ticket"
xcrun stapler staple "$APP_BUNDLE"

echo "==> Validating staple + Gatekeeper assessment"
xcrun stapler validate "$APP_BUNDLE"
spctl --assess --type execute --verbose=4 "$APP_BUNDLE" || \
    echo "    (spctl assessment non-zero — review output above)"

rm -f "$ZIP_PATH"
echo "==> Signed + notarized: $APP_BUNDLE"
echo "    Next: packaging/macos/make_dmg.sh to wrap it in a distributable DMG."
