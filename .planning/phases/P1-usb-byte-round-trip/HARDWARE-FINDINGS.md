# P1 — Hardware Validation Findings (live M1 ↔ Pixel 6a session)

**Date:** 2026-06-03
**Setup:** M1 MacBook (USB host) ↔ Pixel 6a (Android, USB device) over USB-C.
**Status:** Core viability **PROVEN**; end-to-end byte echo **not yet completed** (one device-side handoff issue remains). XPORT-01 partially met — see verdict.

## D1 verdict (so far): **AOA is viable**
The Android Open Accessory path works from Rust on macOS. The NCM/TCP fallback was not needed for viability (and its phone-side TCP server is not yet wired — see "Open items").

## What was PROVEN on real hardware ✅
1. **AOA handshake from Rust/macOS** — `nusb` device-level control transfers req 51/52/53 succeed; the Pixel reports **AOA protocol v2**.
2. **No `sudo` / no entitlement needed** (resolves the macOS unknown **A2**). The key was doing the handshake via `Device::control_*` rather than claiming interface 0.
3. **Re-enumeration into accessory mode** works — phone switches `18d1:4ee2` → `18d1:2d01` (accessory+ADB); the Mac re-acquires and claims the accessory interface cleanly (no sudo).
4. **App receives the fd** — the Android app loads the (16 KB-aligned) `.so`, and `nativeOnUsbFd` runs the echo loop (observed once in logcat: "received accessory fd 99, starting echo loop").

## Bugs found & fixed during the session
- **macOS `kIOReturnExclusiveAccess` (0xe00002c5)** claiming interface 0 → fixed by doing the AOA handshake with **device-level control transfers** (no interface claim); only the *accessory* interface (driverless) is claimed, post-re-enumeration. No sudo.
- **16 KB ELF alignment** — `libandroid_client.so` LOAD segments were 4 KB-aligned; Android 15/Pixel flagged "ELF alignment check failed". Fixed via `.cargo/config.toml` (`-Wl,-z,max-page-size=16384`); verified segments now `0x4000`-aligned.
- **Echo loop blocked the UI thread** → ANR. Fixed: `nativeOnUsbFd` now runs on a background thread (Kotlin `Thread{}`).
- **App never requested accessory permission** — relied solely on the per-install auto-grant from the `USB_ACCESSORY_ATTACHED` intent, which resets on reinstall. Fixed: `MainActivity` now `requestPermission()`s (shows the "Allow?" dialog) and falls back to `accessoryList`.

## External interference discovered
- **Android Auto** (`com.google.android.projection.gearhead`) intercepts the AOA `USB_ACCESSORY_HANDSHAKE` (Android 12+ car-detection), delaying it ~10 s and racing our app. Disabling it removes the interference.
- **Charge-only USB cable** is a hard prerequisite trap — a power-only cable enumerates nothing on the host (symptom: greyed-out "Use USB for" options + empty `system_profiler SPUSBDataType`).

## Open item (the one remaining blocker for a green echo)
After re-enumeration, the Mac claims the accessory interface **immediately**, and Android's `accessoryList` is then **empty** when the app checks — so the app can't open the accessory and no bytes round-trip (the host's `echo_roundtrip` waits and times out). Leading hypothesis: a **device-side race** — the host should *delay* claiming the interface (and/or retry) until Android has registered the accessory and handed it to the app. This is the focused next step ("option B").

## Repro / runbook (for the next session)
1. Data USB-C cable, phone unlocked, USB debugging on, "File transfer" mode.
2. `cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client && (cd android && ./gradlew assembleDebug) && adb install -r android/app/build/outputs/apk/debug/app-debug.apk`
3. (Optional) disable Android Auto: `adb shell pm disable-user --user 0 com.google.android.projection.gearhead` (re-enable with `pm enable` after).
4. `cargo build -p macos-host --features live-usb`
5. `adb kill-server && target/debug/p1_echo` → open the RustScreen app, tap "Allow USB accessory".
6. Logs: `adb logcat -d | grep -iE "RustScreen|echo_loop|nativeOnUsbFd"`.

## Notes
- `p1_echo` currently carries debug instrumentation (verbose handshake/reacquire logs, a 32-byte warm-up echo, `ITERS=4`) added during this session; trim before declaring P1 done.
- macОS only enumerates Android USB with a **data** cable; no Mac driver/setting is involved.
