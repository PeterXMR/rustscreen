# macOS packaging (RustScreen.app + DMG)

P8 packaging scaffold for the macOS host. This directory builds a `.app` bundle,
signs + notarizes it under the hardened runtime, and wraps it in a distributable
DMG.

> **Status — bundles the real CLI, signing not yet validated.** `cargo xtask make-app`
> now builds and bundles the live `rustscreen` CLI (features `live-capture,live-usb`
> by default — previously it shipped a do-nothing version-printing stub). The packaging
> path has **not** been run through a real Apple Developer signing identity, and the
> bundle has no double-click launch behavior yet (`rustscreen` without arguments prints
> usage; the Dock/menu-bar shape is roadmap P7). The signing/notarization step in
> particular depends on the entitlement set, which is an **open question** (see below).

## Commands & files

The build/release steps are `cargo xtask` subcommands (run from the repo root); the
data files they consume live in this directory.

| Command / file | Purpose |
|---|---|
| `cargo xtask make-app` | Release-build the `rustscreen` CLI (`live-capture,live-usb` by default) and assemble `dist/RustScreen.app`. No signing. |
| `cargo xtask sign-notarize` | Codesign the bundle (hardened runtime) → submit to Apple notary → staple. |
| `cargo xtask make-dmg` | Wrap the (signed) `.app` in `dist/RustScreen-<version>.dmg` with an `/Applications` drop-link. |
| `Info.plist` | Bundle descriptor template (`__VERSION__` substituted at build time). |
| `entitlements.plist` | Hardened-runtime entitlements — **first guess, refine empirically**. |

## Quick start (once you have a Developer ID certificate)

```bash
# 1. Assemble the bundle (defaults to --features live-capture,live-usb — the live host;
#    pass --features explicitly only to extend, e.g. adding live-inject):
cargo xtask make-app --features live-capture,live-usb,live-inject

# 2. Sign + notarize (needs a "Developer ID Application" cert + a notarytool profile).
#    Create the notarytool profile once:
#      xcrun notarytool store-credentials rustscreen-notary \
#        --apple-id you@example.com --team-id TEAMID --password <app-specific-pw>
SIGN_IDENTITY="Developer ID Application: Your Name (TEAMID)" \
NOTARY_PROFILE="rustscreen-notary" \
cargo xtask sign-notarize

# 3. Build the DMG.
cargo xtask make-dmg
# → dist/RustScreen-0.1.0.dmg
```

No secrets live in the repo: the signing identity and notary credentials are
supplied at run time via env vars / a keychain profile.

## Distribution channel: notarized DMG, NOT the Mac App Store

RustScreen creates a virtual display via the **private** `CGVirtualDisplay` ObjC
class. The Mac App Store forbids private APIs, so distribution is via a **notarized
DMG** (architecture roadmap, P2 risk). Notarization checks code signing and malware
— **not** private-API use — so a correctly-signed binary is expected to notarize
(risk register **R8**, likelihood *Low*).

## ⚠️ Open question: the hardened-runtime entitlement set

`entitlements.plist` is a **starting point**, not a verified set. It must be pinned
down empirically by running the signed host on a real Mac under the hardened runtime
and checking whether the private `CGVirtualDisplay` symbols resolve. Notes:

- **`com.apple.security.cs.disable-library-validation`** is enabled as a
  conservative first guess for private-CoreGraphics symbol resolution. Remove it if
  testing shows it is unnecessary; keep it minimal.
- **Screen recording** (ScreenCaptureKit) and **Accessibility** (CGEvent injection)
  are **TCC permissions** granted by the user at runtime — they are *not*
  entitlements and need no key here.
- **USB** (`nusb`/AOA) needs no `com.apple.security.device.usb` entitlement on a
  notarized non-MAS app (that key is MAS-sandbox only).

Once verified, replace the comment block at the top of `entitlements.plist` with the
confirmed minimal set and the macOS version it was tested on.

## TODO before this is shippable

- [ ] Run `cargo xtask sign-notarize` against a real Developer ID cert; confirm the app
      notarizes and Gatekeeper passes (`spctl --assess`).
- [ ] Confirm the private `CGVirtualDisplay` path works under the hardened runtime;
      finalize `entitlements.plist`.
- [ ] Add `AppIcon.icns` (drop it in this directory; `cargo xtask make-app` picks it up).
- [ ] Give the bundle a double-click launch behavior (menu-bar status item, roadmap P7) —
      the bundled `rustscreen` CLI currently prints usage when run without arguments.
