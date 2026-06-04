# P1 — Hardware Validation Findings (live M1 ↔ Pixel 6a session)

**Date:** 2026-06-03 → 2026-06-04
**Setup:** M1 MacBook (USB host) ↔ Pixel 6a (Android, USB device) over USB-C (data cable).
**Status:** ✅ **COMPLETE — end-to-end byte echo PROVEN green.** XPORT-01 met.

## D1 verdict: **AOA (Android Open Accessory)** — chosen transport
The full Mac↔Pixel byte round-trip works from Rust on macOS over AOA bulk endpoints, with
**no sudo and no entitlement**. The NCM/TCP path stays documented as a fallback but was never
needed for viability. **XPORT-01 closes on AOA.**

### Measured result (production transport code)
```
device hello received (0 bytes) — app reader is live
warm-up OK: 32B frame echoed byte-for-byte
echo 0..3: 1048576 bytes ok   (byte-for-byte)
P1 echo OK: 4 MiB byte-for-byte over 4 iters; throughput 103.0 Mbit/s
```
- **Throughput: ~103 Mbit/s (~12.9 MB/s) sustained** over AOA bulk, M1 ↔ Pixel 6a.
- This sits in the known AOA bulk band (~100–150 Mbit/s) and leaves **4–8× headroom** over a
  1080p60 H.264 stream (~10–25 Mbit/s) — comfortably sufficient for the video path.
- Measurement was the verified live run (4 × 1 MiB). The committed `p1_echo` was then trimmed
  (`ITERS` 4→16, reacquire 20s→5s, quieter logs) — non-behavioral; byte-exactness and the
  throughput *rate* are unchanged.

## What was PROVEN on real hardware ✅
1. **AOA handshake from Rust/macOS** — `nusb` device-level control transfers req 51/52/53
   succeed; the Pixel reports **AOA protocol v2**.
2. **No `sudo` / no entitlement needed** (resolves macOS unknown **A2**). The handshake goes
   via `Device::control_*` rather than claiming interface 0.
3. **Re-enumeration into accessory mode** — phone switches `18d1:4ee2` → `18d1:2d01`
   (accessory+ADB); the Mac re-acquires and claims the *accessory* interface cleanly (no sudo).
4. **App receives the fd and echoes** — the app loads the 16 KB-aligned `.so`, `nativeOnUsbFd`
   takes sole ownership of the detached fd and runs the framed `echo_loop`.
5. **Full bidirectional byte-exact echo** — 32-byte warm-up + 4× 1 MiB frames round-trip
   byte-for-byte at 103 Mbit/s.

## Live bugs found & fixed (the two that gated a green echo)
1. **Startup-ordering deadlock (host→device).** The host wrote its first bulk-OUT ~2–4 s
   *before* the app opened `/dev/usb_accessory`; the gadget dropped those bytes and both sides
   hung. **Fix — connect-hello handshake:** the device sends a one-frame hello (tag 2) the
   instant it owns the accessory fd; the host blocks reading that hello *before* it writes, so
   its first OUT can't land before the app's reader is live. (Device→host bulk IN buffers
   reliably, so "device speaks first" is the robust ordering.)
2. **host→device bulk-OUT truncation (THE last blocker).** On the Android `f_accessory`
   gadget, the size passed to `read()` bounds the USB OUT request — the framing codec's small
   `read_exact(5)` sized the OUT URB to 5 bytes and **silently truncated** the host's larger
   bulk packet (host transfer still ACKs; device keeps only the first few bytes), deadlocking
   the round-trip. **Fix — `AccessoryFdTransport` buffers reads:** it always issues a 16 KiB
   (`ACCESSORY_READ_CHUNK ≥ host BULK_CHUNK`) read into an internal buffer and serves the
   codec's small reads from there. The 32-byte warm-up completing is the direct proof.

## Bugs fixed earlier in the session (pre-green)
- **macOS `kIOReturnExclusiveAccess` (0xe00002c5)** claiming interface 0 → fixed by doing the
  AOA handshake with **device-level control transfers** (no interface claim); only the
  driverless *accessory* interface is claimed, post-re-enumeration. No sudo.
- **16 KB ELF alignment** — `libandroid_client.so` LOAD segments were 4 KB-aligned; Android
  15/Pixel flagged "ELF alignment check failed". Fixed via `.cargo/config.toml`
  (`-Wl,-z,max-page-size=16384`); segments now `0x4000`-aligned.
- **Echo loop blocked the UI thread** → ANR. Fixed: `nativeOnUsbFd` runs on a background
  thread (Kotlin `Thread{}`).
- **App never requested accessory permission** — relied on the per-install auto-grant from the
  `USB_ACCESSORY_ATTACHED` intent, which resets on reinstall. Fixed: `MainActivity`
  `requestPermission()`s (shows the "Allow?" dialog) and falls back to `accessoryList`.

## External interference discovered
- **Android Auto** (`com.google.android.projection.gearhead`) intercepts the AOA
  `USB_ACCESSORY_HANDSHAKE` (Android 12+ car detection), delaying it ~10 s and racing our app.
  Disable for the session: `adb shell pm disable-user --user 0 com.google.android.projection.gearhead`
  (re-enable with `pm enable` after). It re-enables itself across reboots/updates — re-check.
- **Charge-only USB cable** is a hard prerequisite trap — a power-only cable enumerates nothing
  on the host (symptom: greyed-out "Use USB for" options + empty `system_profiler SPUSBDataType`).

## Repro / runbook
1. Data USB-C cable, phone unlocked, USB debugging on, "File transfer" mode.
2. `cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client && (cd android && ./gradlew assembleDebug) && adb install -r android/app/build/outputs/apk/debug/app-debug.apk`
3. (Recommended) disable Android Auto: `adb shell pm disable-user --user 0 com.google.android.projection.gearhead`.
4. `cargo build -p macos-host --features live-usb`
5. `adb kill-server && target/debug/p1_echo` → launch the app (`adb start-server && adb shell am start -n com.rustscreen.client/.MainActivity`) → tap **Allow** on the accessory dialog (tick "use by default" to skip future prompts).
6. Expect: `device hello received` → `warm-up OK` → `echo N: 1048576 bytes ok` → `P1 echo OK … throughput … Mbit/s`.
7. To re-run: **replug the cable** first (resets the phone from accessory mode `2d01` back to
   normal mode `4ee2` so `find_candidate` can re-handshake).
8. Logs: host → `/tmp/p1_echo.log`; phone → `adb logcat -d | grep -iE "RustScreen|echo_loop|nativeOnUsbFd"`.

## Notes
- macOS only enumerates Android USB with a **data** cable; no Mac driver/setting is involved.
- 1 MiB = 512 × 2048 is an exact multiple of the max packet size; `nusb` `flush()` uses
  `submit()` (no ZLP), and the framing reader reads exact lengths, so no zero-length packet is
  needed at the app level. (Flagged here in case a future non-multiple payload needs a ZLP.)
