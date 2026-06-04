# Android packaging (release APK + signing)

P8 packaging scaffold for the Android client. This covers building a **release** APK
(release-profile Rust `.so` + Gradle `assembleRelease`), the release-signing config,
and the USB-accessory permission the app needs at install time.

> **Status — scaffold.** The release build path and signing config are wired, but no
> real keystore exists and the on-device pipeline (P4 decode, P5 live wiring) isn't
> done — so a release APK installs and launches the thin shell, but does not yet
> render a second screen. Play Store distribution is **optional / undecided**
> (architecture roadmap P8).

## Build a release APK

```bash
# One-time setup (same as the debug build — see ../../CONTRIBUTING.md):
rustup target add aarch64-linux-android
cargo install cargo-ndk
export ANDROID_HOME="$HOME/Library/Android/sdk"
export ANDROID_NDK_HOME="$ANDROID_HOME/ndk/25.2.9519653"

# Build (cross-compile the release .so, then assemble the release APK):
packaging/android/build_release_apk.sh
# → android/app/build/outputs/apk/release/app-release.apk
```

The Gradle release build uses the release Rust profile (`[profile.release]` in the
workspace `Cargo.toml`: size-optimized, LTO, stripped) for the `.so`.

## Release signing

No keystore or password is committed. The signing config in
`android/app/build.gradle.kts` reads credentials from, in order:

1. **Environment variables** (preferred for CI):
   - `RUSTSCREEN_KEYSTORE` — path to the keystore (relative to `android/app/`)
   - `RUSTSCREEN_KEYSTORE_PASSWORD`
   - `RUSTSCREEN_KEY_ALIAS`
   - `RUSTSCREEN_KEY_PASSWORD`
2. **`android/keystore.properties`** (personal, git-ignored) — see
   `keystore.properties.example` here for the keys.

If neither is configured, `assembleRelease` falls back to the **debug** key and
prints a warning — usable for a smoke test, **not** for distribution.

### Generate a keystore (once)

```bash
keytool -genkeypair -v \
  -keystore release.keystore \
  -alias rustscreen \
  -keyalg RSA -keysize 4096 -validity 10000
```

Keep `release.keystore` and its passwords **out of git** (the `.gitignore` in this
directory and the root one both exclude `*.keystore`). Then either export the env
vars above or copy `keystore.properties.example` → `android/keystore.properties` and
fill it in.

## USB-accessory permission (already declared)

The Android client is a **USB accessory** client (it receives the AOA stream from the
Mac). The capability is **already declared** in
`android/app/src/main/AndroidManifest.xml`:

- `<uses-feature android:name="android.hardware.usb.accessory" android:required="true" />`
  — declares the USB-accessory capability. Without it some Android versions return an
  empty `UsbManager.getAccessoryList()`, and the Play Store would not filter to
  capable devices.
- An intent-filter on `android.hardware.usb.action.USB_ACCESSORY_ATTACHED` plus a
  `<meta-data>` pointing at `res/xml/accessory_filter.xml`, which matches the AOA
  identity strings sent by the host (`crates/macos-host/src/aoa.rs`).

There is **no install-time `<uses-permission>`** to add for USB accessory — Android
grants access through a **runtime per-connection dialog** ("Allow the app to access
the USB accessory?") when the cable is plugged in. Tick "Use by default for this USB
accessory" to suppress it on subsequent connects (roadmap risk R7). No further
manifest change is needed for the release build; this section documents the existing
setup so packagers know the permission model.

## TODO before this is shippable

- [ ] Generate a real release keystore; verify `assembleRelease` produces a
      release-signed APK (`apksigner verify --print-certs`).
- [ ] Decide Play Store vs. sideload distribution (roadmap P8, open).
- [ ] Land P4 (on-device decode) + P5 (live wiring) so the APK renders a screen.
