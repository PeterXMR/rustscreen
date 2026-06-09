# NDK-native Surface Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Move the surface/window/lifecycle half of `MainActivity.kt` into a Rust `NativeActivity` event loop (android-activity), keeping a thin Kotlin `NativeActivity` subclass only for the USB-accessory permission flow.

**Architecture:** A new Rust `android_main` event loop (android-activity, native-activity backend) feeds the *existing* source-agnostic `SURFACE_SLOT` + `SURFACE_MAILBOX` channels from `InitWindow`/`TerminateWindow` events, plus a retained `CURRENT_WINDOW` so a reconnect re-binds without a new OS surface event. The two surface JNI functions are deleted; the USB fd handoff (`nativeOnUsbFd`) and the entire decode pipeline are untouched.

**Tech Stack:** Rust (cdylib), `android-activity` 0.6 (native-activity), `ndk` 0.9, `ndk-sys` 0.6, `jni` 0.21, Kotlin (`android.app.NativeActivity` subclass), Gradle.

---

## Why TDD looks different here

The changed code is **FFI/Android glue** — it cannot run on the host (no `ANativeWindow`, no `AMediaCodec`, no Android looper). The host-testable *logic* it depends on (the `WindowSlot` rendezvous and `SurfaceMailbox` swap channel) is **already fully unit-tested** in `crates/android-client/src/rendezvous.rs` and stays unchanged. So the verification gates here are:

1. **Compile gate** — `cargo ndk -t arm64-v8a build -p android-client --features live-decode` (the real cross-compile, exercises the FFI types).
2. **Host workspace gate** — `cargo test --workspace` still green (the removed fns are `cfg(target_os="android")`, so host CI is unaffected; this confirms no accidental host breakage).
3. **On-device gate** — the device checklist in Task 6.

This mirrors how the repo already treats `mediacodec.rs` (hardware-blocked, behind `live-decode`, not host-tested).

## The reconnect correctness gap (design-critical)

Today the Kotlin shell re-deposits its tracked `currentSurface` at the start of **every** session (`openAndRun`: `currentSurface?.let { nativeOnSurface(it) }`). This is what lets a **reconnect** (a new USB session while the process + surface stay alive — e.g. replug/EOF auto-reconnect) bind a surface, because `SURFACE_SLOT` is consume-once and was drained by the previous session.

With `android_main` owning surface events, `InitWindow` fires only when the OS creates/recreates the window (app start, or foreground after a real destroy) — **not** on a USB-only reconnect. So the new session would find an empty `SURFACE_SLOT` and time out.

**Fix:** `android_main` retains the latest live window in a `CURRENT_WINDOW: Mutex<Option<NativeWindow>>` (set on `InitWindow`, cleared on `TerminateWindow`). `run_usb_fd` re-seeds `SURFACE_SLOT` from `CURRENT_WINDOW` before blocking — exactly mirroring the Kotlin re-deposit. Cold launch-by-plug (fd before the first `InitWindow`) finds `CURRENT_WINDOW` empty and still waits for `InitWindow` to `put()`.

---

## File Structure

| File | Responsibility after this change |
|---|---|
| `crates/android-client/Cargo.toml` | Pull `android-activity`/`ndk` only under `live-decode` (keeps default workspace build dependency-free) |
| `crates/android-client/src/mediacodec.rs` | Add `NativeWindow::from_ptr_acquire` (build an owned window from android-activity's raw pointer) |
| `crates/android-client/src/lib.rs` | Add `android_main` event loop + `CURRENT_WINDOW`; re-seed slot in `run_usb_fd`; delete `nativeOnSurface`/`nativeOnSurfaceDestroyed` |
| `android/app/src/main/java/com/rustscreen/client/MainActivity.kt` | `NativeActivity` subclass; USB glue only (no SurfaceView) |
| `android/app/src/main/AndroidManifest.xml` | `android.app.lib_name` meta-data; keep USB intent-filters + orientation |

---

## Task 1: Add android-activity + ndk dependencies (live-decode only)

**Files:**
- Modify: `crates/android-client/Cargo.toml`

- [ ] **Step 1: Extend the `live-decode` feature and add the optional deps**

In `crates/android-client/Cargo.toml`, change the `live-decode` feature line:

```toml
# (was) live-decode = ["dep:ndk-sys"]
live-decode = ["dep:ndk-sys", "dep:android-activity", "dep:ndk"]
```

And in `[target.'cfg(target_os = "android")'.dependencies]`, add below the existing `ndk-sys` line:

```toml
# NativeActivity entry point + event loop (surface/lifecycle). native-activity backend pulls
# NO AndroidX (unlike the GameActivity backend), preserving the app's zero-AndroidX profile.
# Optional + only via `live-decode`, exactly like ndk-sys, so the default host workspace build
# stays dependency-free.
android-activity = { version = "0.6", features = ["native-activity"], optional = true }
# High-level `ndk` wrapper — used only to read the raw `ANativeWindow` pointer out of
# android-activity's `AndroidApp::native_window()`. Pinned to the same ndk-sys 0.6 ABI.
ndk = { version = "0.9", optional = true }
```

- [ ] **Step 2: Resolve & lock the dependency graph**

Run: `cargo update -p android-client --dry-run 2>&1 | head -5; cargo metadata --format-version 1 --filter-platform aarch64-linux-android >/dev/null && echo METADATA_OK`
Expected: `METADATA_OK` (the graph resolves; android-activity 0.6 unifies on ndk-sys 0.6).

- [ ] **Step 3: Confirm ndk-sys versions unify (no duplicate ANativeWindow type)**

Run: `cargo tree -p android-client --target aarch64-linux-android -i ndk-sys 2>/dev/null | head -20`
Expected: a single `ndk-sys v0.6.x` node (android-activity, ndk, and android-client all point at it). If two ndk-sys majors appear, pin them before proceeding.

- [ ] **Step 4: Commit**

```bash
git add crates/android-client/Cargo.toml Cargo.lock
git commit -m "build(android-client): add android-activity + ndk under live-decode"
```

---

## Task 2: Add `NativeWindow::from_ptr_acquire`

**Files:**
- Modify: `crates/android-client/src/mediacodec.rs` (after `clone_acquire`, ~line 119)

- [ ] **Step 1: Add the constructor**

Insert this method inside `impl NativeWindow { ... }`, right after `clone_acquire`:

```rust
    /// Acquire an owned `ANativeWindow` from a raw pointer — the window android-activity
    /// surfaces via `AndroidApp::native_window()` in the [`crate::android_main`] event loop
    /// (the NativeActivity path that replaces the Kotlin `SurfaceView` + `from_surface`).
    /// Adds our own reference with `ANativeWindow_acquire`, so android-activity's handle stays
    /// independently owned and this one is released exactly once on `Drop`. `None` for null.
    ///
    /// # Safety
    /// `ptr` must be null or a valid, live `ANativeWindow` — true for the pointer android-activity
    /// exposes while a window is current (between `InitWindow` and `TerminateWindow`).
    pub unsafe fn from_ptr_acquire(ptr: *mut sys::ANativeWindow) -> Option<Self> {
        let ptr = NonNull::new(ptr)?;
        // SAFETY: `ptr` is a live `ANativeWindow` (caller contract); `acquire` bumps its
        // refcount and the returned handle releases exactly that reference on `Drop`.
        sys::ANativeWindow_acquire(ptr.as_ptr());
        Some(NativeWindow { ptr })
    }
```

- [ ] **Step 2: Verify it compiles for the android target**

Run: `cargo ndk -t arm64-v8a build -p android-client --features live-decode 2>&1 | tail -15`
Expected: builds (a warning that `from_ptr_acquire` is unused is fine — Task 3 calls it). If `cargo ndk` errors on the NDK path, set `ANDROID_NDK_HOME` first.

- [ ] **Step 3: Commit**

```bash
git add crates/android-client/src/mediacodec.rs
git commit -m "feat(android-client): NativeWindow::from_ptr_acquire for the NativeActivity path"
```

---

## Task 3: Add `android_main` + `CURRENT_WINDOW`; delete surface JNI; re-seed slot

**Files:**
- Modify: `crates/android-client/src/lib.rs`

This is the core task. It (a) adds the retained window + event loop, (b) re-seeds the consume-once slot from it, (c) removes the two surface JNI functions.

- [ ] **Step 1: Add `CURRENT_WINDOW` next to `SURFACE_SLOT`**

In `lib.rs`, find the `SURFACE_SLOT` static (~line 260, inside `mod android`, under `#[cfg(feature = "live-decode")]`) and add directly after it:

```rust
    /// The latest live render window, retained by [`android_main`] across its lifetime (set on
    /// `InitWindow`, cleared on `TerminateWindow`). `SURFACE_SLOT` is consume-once, so a *new*
    /// session that starts while the window already exists (a USB reconnect — replug/EOF — with
    /// the process and surface still alive) would find the slot drained and time out. `run_usb_fd`
    /// re-seeds the slot from this before blocking, exactly mirroring the old Kotlin shell's
    /// per-session `currentSurface` re-deposit. A cold launch-by-plug (fd before the first
    /// `InitWindow`) finds this empty and instead waits for `InitWindow` to `put()`.
    #[cfg(feature = "live-decode")]
    static CURRENT_WINDOW: std::sync::Mutex<Option<crate::mediacodec::NativeWindow>> =
        std::sync::Mutex::new(None);
```

- [ ] **Step 2: Re-seed the slot in `run_usb_fd` before the blocking take**

In `run_usb_fd` (the `#[cfg(feature = "live-decode")]` variant, ~line 206), replace the rendezvous block:

```rust
        // Rendezvous: the surface arrives on a separate callback, possibly after the fd.
        let window = SURFACE_SLOT
            .take_blocking(Duration::from_secs(10))
            .ok_or("no render surface within 10s (SurfaceView never created?)")?;
        log::info!("nativeOnUsbFd: surface acquired; decoding to surface");
```

with:

```rust
        // Re-seed the consume-once slot from the retained live window so a reconnect (a new
        // session with the surface already created and the slot long since drained) binds
        // immediately — mirrors the old Kotlin shell's per-session re-deposit. A cold
        // launch-by-plug (fd before the first InitWindow) finds CURRENT_WINDOW empty here and
        // instead waits below for android_main to put() the window once the OS creates it.
        if let Some(w) = CURRENT_WINDOW.lock().unwrap().as_ref() {
            SURFACE_SLOT.put(w.clone_acquire());
        }
        // Rendezvous: the window arrives from android_main's InitWindow, possibly after the fd.
        let window = SURFACE_SLOT
            .take_blocking(Duration::from_secs(10))
            .ok_or("no render surface within 10s (NativeActivity window never created?)")?;
        log::info!("nativeOnUsbFd: surface acquired; decoding to surface");
```

- [ ] **Step 3: Replace the two surface JNI fns with `android_main`**

Delete the entire `Java_com_rustscreen_client_MainActivity_nativeOnSurface` and
`Java_com_rustscreen_client_MainActivity_nativeOnSurfaceDestroyed` functions (and their doc
comments), ~lines 264–326. Replace them with the `android_main` event loop:

```rust
    /// NativeActivity entry point (android-activity, native-activity backend). Replaces the
    /// Kotlin `SurfaceView` + the `nativeOnSurface`/`nativeOnSurfaceDestroyed` JNI calls: the OS
    /// surface lifecycle now arrives here as `InitWindow`/`TerminateWindow` events, and we feed
    /// the SAME two channels the decode session already consumes — so every downstream behavior
    /// (cold-launch rendezvous via `SURFACE_SLOT`, and the live background→foreground swap via
    /// `SURFACE_MAILBOX`, PR #36) is preserved unchanged. The USB fd handoff stays in Kotlin
    /// (`nativeOnUsbFd`) because `UsbManager` has no NDK equivalent.
    ///
    /// Runs on android-activity's dedicated main thread for the activity's lifetime; the blocking
    /// decode runs on the separate USB thread the Kotlin shell spawns.
    #[cfg(feature = "live-decode")]
    #[no_mangle]
    fn android_main(app: android_activity::AndroidApp) {
        use android_activity::{MainEvent, PollEvent};
        use crate::mediacodec::{NativeWindow, SURFACE_MAILBOX};
        use std::time::Duration;

        // Idempotent: the logger is also init'd by nativeInit from Kotlin onCreate; init here too
        // so events logged from this thread are captured regardless of thread start ordering.
        android_logger::init_once(
            android_logger::Config::default().with_max_level(log::LevelFilter::Info),
        );
        log::info!("android_main: NativeActivity event loop started");

        let mut running = true;
        while running {
            app.poll_events(Some(Duration::from_millis(250)), |event| {
                if let PollEvent::Main(main) = event {
                    match main {
                        // Surface available (app start, or foreground after a real destroy).
                        MainEvent::InitWindow { .. } => {
                            let Some(ndk_win) = app.native_window() else {
                                log::error!("android_main: InitWindow but native_window() is None");
                                return;
                            };
                            // SAFETY: the pointer is a live ANativeWindow for the duration of this
                            // InitWindow..TerminateWindow span; from_ptr_acquire adds our own ref.
                            let Some(window) =
                                (unsafe { NativeWindow::from_ptr_acquire(ndk_win.ptr().as_ptr()) })
                            else {
                                log::error!("android_main: native window pointer was null");
                                return;
                            };
                            log::info!("android_main: InitWindow — depositing render window");
                            // Mirror the old nativeOnSurface: feed BOTH the live swap mailbox
                            // (a running session re-points onto it) and the consume-once slot (a
                            // fresh session's initial bind), and retain it for reconnect re-seed.
                            SURFACE_MAILBOX.deposit(window.clone_acquire());
                            *CURRENT_WINDOW.lock().unwrap() = Some(window.clone_acquire());
                            SURFACE_SLOT.put(window);
                        }
                        // Surface destroyed (app backgrounded). Mirror nativeOnSurfaceDestroyed.
                        MainEvent::TerminateWindow { .. } => {
                            log::info!("android_main: TerminateWindow — retracting render window");
                            SURFACE_SLOT.clear();
                            SURFACE_MAILBOX.mark_lost();
                            *CURRENT_WINDOW.lock().unwrap() = None;
                        }
                        // Activity going away: drop the retained window and exit the loop.
                        MainEvent::Destroy => {
                            log::info!("android_main: Destroy — exiting event loop");
                            *CURRENT_WINDOW.lock().unwrap() = None;
                            running = false;
                        }
                        _ => {}
                    }
                }
            });
        }
    }
```

- [ ] **Step 4: Update the module-level doc comment**

At the top of `lib.rs` (lines 1-3), replace:

```rust
//! RustScreen Android client (cdylib). JNI entry points called by the thin Kotlin shell (D7).
//! All app logic lives here in Rust; the Kotlin shell is glue only and will be replaced by
//! NativeActivity in a later phase.
```

with:

```rust
//! RustScreen Android client (cdylib). The render surface + activity lifecycle run here as a
//! NativeActivity event loop (`android_main`, android-activity); the remaining thin Kotlin shell
//! (a `NativeActivity` subclass) does only the USB-accessory permission flow — which has no NDK
//! equivalent — and hands the accessory fd to `nativeOnUsbFd`. All pipeline logic is Rust.
```

- [ ] **Step 5: Build for android with the surface JNI removed**

Run: `cargo ndk -t arm64-v8a build -p android-client --features live-decode 2>&1 | tail -20`
Expected: clean build. No references to `nativeOnSurface`/`nativeOnSurfaceDestroyed` remain (`grep -rn nativeOnSurface crates/android-client/src` returns nothing).

- [ ] **Step 6: Confirm the host workspace is unaffected**

Run: `cargo test --workspace 2>&1 | tail -15`
Expected: all tests pass (rendezvous + mailbox tests unchanged; the android module isn't compiled on host).

- [ ] **Step 7: Clippy the android target**

Run: `cargo ndk -t arm64-v8a clippy -p android-client --features live-decode 2>&1 | tail -20`
Expected: no warnings (unused-import check too — Commit Hygiene rule).

- [ ] **Step 8: Commit**

```bash
git add crates/android-client/src/lib.rs
git commit -m "feat(android-client): android_main NativeActivity event loop; drop surface JNI

Feed SURFACE_SLOT + SURFACE_MAILBOX from InitWindow/TerminateWindow instead of the
Kotlin nativeOnSurface JNI calls. Retain CURRENT_WINDOW so a USB reconnect re-binds
the surface (mirrors the old per-session Kotlin re-deposit). PR #36 background swap
and the cold-launch rendezvous are preserved unchanged."
```

---

## Task 4: Shrink `MainActivity.kt` to a NativeActivity subclass

**Files:**
- Modify: `android/app/src/main/java/com/rustscreen/client/MainActivity.kt`

Keep ALL USB-accessory logic (permission receiver, poll, `pickOurAccessory`, `openAndRun`, the
session thread + `nativeOnUsbFd`, the Bye-close path). Remove the SurfaceView, the
`SurfaceHolder.Callback`, `currentSurface`, `setContentView`, and the `nativeOnSurface*`
externals.

- [ ] **Step 1: Change the base class and imports**

Change the class declaration:

```kotlin
// (was) class MainActivity : Activity() {
class MainActivity : android.app.NativeActivity() {
```

Remove these now-unused imports:

```kotlin
import android.app.Activity
import android.view.Surface
import android.view.SurfaceHolder
import android.view.SurfaceView
```

(Keep `android.view.WindowManager` — still used for `FLAG_KEEP_SCREEN_ON`.)

- [ ] **Step 2: Delete the `currentSurface` field**

Remove the `@Volatile private var currentSurface: Surface? = null` field and its doc comment
(~lines 37-45).

- [ ] **Step 3: Rewrite `onCreate` — drop the SurfaceView, keep USB setup**

Replace the `onCreate` body. `super.onCreate()` now boots NativeActivity (which loads the
native lib via the `android.app.lib_name` meta-data and starts `android_main`). We no longer
create a SurfaceView or call `setContentView` — NativeActivity owns the window.

```kotlin
    override fun onCreate(savedInstanceState: Bundle?) {
        // NativeActivity.onCreate loads libandroid_client (android.app.lib_name meta-data) and
        // starts the Rust android_main event loop, which owns the render surface. We add ONLY the
        // USB-accessory glue on top — UsbManager has no NDK equivalent, so it stays in Kotlin.
        super.onCreate(savedInstanceState)
        // Keep the phone awake for the whole session — a second screen that sleeps is useless.
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        nativeInit()
        val filter = IntentFilter(ACTION_USB_PERMISSION)
        // Android 13+ requires an explicit export flag for runtime-registered receivers.
        if (Build.VERSION.SDK_INT >= 33) {
            registerReceiver(permissionReceiver, filter, Context.RECEIVER_NOT_EXPORTED)
        } else {
            @Suppress("UnspecifiedRegisterReceiverFlag")
            registerReceiver(permissionReceiver, filter)
        }
        maybeHandleAccessory()
    }
```

- [ ] **Step 4: Remove the surface re-deposit in `openAndRun`**

In `openAndRun`, delete the re-deposit block (the Rust `CURRENT_WINDOW` now does this):

```kotlin
        // DELETE THIS BLOCK:
        currentSurface?.let { surface ->
            Log.i(TAG, "re-depositing existing surface for new decode session")
            nativeOnSurface(surface)
        }
```

(The surrounding comment paragraph about re-depositing goes too.)

- [ ] **Step 5: Remove the surface JNI externals**

In the `companion object`, delete the `nativeOnSurface` and `nativeOnSurfaceDestroyed`
declarations and their doc comments (~lines 295-302). Also delete the
`System.loadLibrary("android_client")` line from the `init { }` block — NativeActivity loads the
lib via the `android.app.lib_name` meta-data (Task 5), so a second explicit load is redundant.
Keep `nativeInit` and `nativeOnUsbFd`.

The companion `init { }` block becomes empty; remove it entirely. Result:

```kotlin
    companion object {
        private const val TAG = "RustScreen"
        private const val ACTION_USB_PERMISSION = "com.rustscreen.client.USB_PERMISSION"
        private const val ACCESSORY_POLL_MS = 250L
        private const val EXPECTED_MANUFACTURER = "RustScreen"

        @JvmStatic
        external fun nativeInit()

        // Returns true iff the session ended because the host sent Control::Bye (`rustscreen stop`),
        // so the caller closes the app. EOF/disconnect (replug) and errors return false → stay alive.
        @JvmStatic
        external fun nativeOnUsbFd(fd: Int): Boolean
    }
```

(Preserve the existing `ACCESSORY_POLL_MS` and `EXPECTED_MANUFACTURER` doc comments verbatim.)

- [ ] **Step 6: Confirm no surface references remain**

Run: `grep -n 'Surface\|currentSurface\|setContentView\|loadLibrary' android/app/src/main/java/com/rustscreen/client/MainActivity.kt`
Expected: no matches (all surface handling is gone; lib loads via manifest).

- [ ] **Step 7: Commit**

```bash
git add android/app/src/main/java/com/rustscreen/client/MainActivity.kt
git commit -m "refactor(android): MainActivity is a thin NativeActivity subclass (USB glue only)

Drop the SurfaceView/SurfaceHolder.Callback, currentSurface, setContentView, and the
nativeOnSurface* JNI calls — the Rust android_main event loop owns the render surface now.
Keep the USB-accessory permission flow, foreground poll, session thread, and Bye-close."
```

---

## Task 5: Manifest — point NativeActivity at the native lib

**Files:**
- Modify: `android/app/src/main/AndroidManifest.xml`

- [ ] **Step 1: Add the `android.app.lib_name` meta-data**

Inside the `<activity android:name=".MainActivity" ...>` element, add (e.g. right after the
opening tag's attributes, alongside the existing intent-filters):

```xml
            <!-- NativeActivity loads this lib (libandroid_client.so) and calls its
                 ANativeActivity_onCreate (provided by android-activity) → Rust android_main,
                 which owns the render surface. Value is the lib name WITHOUT the lib prefix /
                 .so suffix. Required because MainActivity extends NativeActivity. -->
            <meta-data
                android:name="android.app.lib_name"
                android:value="android_client" />
```

Keep everything else: the USB `<intent-filter>` + `accessory_filter` meta-data, `singleTop`,
`sensorLandscape`, `configChanges`, `exported`.

- [ ] **Step 2: Sanity-check the manifest**

Run: `grep -n 'lib_name\|android_client\|USB_ACCESSORY_ATTACHED\|sensorLandscape' android/app/src/main/AndroidManifest.xml`
Expected: shows the new `lib_name`/`android_client` plus the preserved USB filter + orientation.

- [ ] **Step 3: Commit**

```bash
git add android/app/src/main/AndroidManifest.xml
git commit -m "build(android): declare android.app.lib_name for NativeActivity"
```

---

## Task 6: Build the APK + on-device verification

**Files:** none (verification only)

- [ ] **Step 1: Build the native lib into jniLibs**

Run: `cargo ndk -t arm64-v8a -o android/app/src/main/jniLibs build -p android-client --release --features live-decode 2>&1 | tail -10`
Expected: `libandroid_client.so` refreshed under `android/app/src/main/jniLibs/arm64-v8a/`.

- [ ] **Step 2: Assemble the APK (JDK 17–21)**

Run: `cd android && ./gradlew assembleDebug 2>&1 | tail -15`
Expected: `BUILD SUCCESSFUL`. (If Gradle fails on JDK 25, select JDK 17–21 first — see the
APK-build memory.)

- [ ] **Step 3: Install + launch on the connected Pixel 6a, then run the host**

Use the `run-on-device` skill (preferred) or:
```bash
adb install -r android/app/build/outputs/apk/debug/app-debug.apk
adb shell am start -W -n com.rustscreen.client/.MainActivity
# then start the host: cargo run -p macos-host --features live ... (rustscreen start)
```

- [ ] **Step 4: Device acceptance checklist (the real gate)**

Confirm each — these are the behaviors the refactor must preserve:
- [ ] **Cold launch-by-plug paints**: plug in with the app closed → app starts, a clean IDR paints (no garbage). (`SURFACE_SLOT` cold rendezvous works via `InitWindow`.)
- [ ] **Already-open launch paints**: app open first, then host starts → paints.
- [ ] **Background→foreground swap, NO reconnect** (PR #36): background the app mid-stream, then foreground → resumes on a fresh IDR with no ~19s reconnect storm. (`SURFACE_MAILBOX` + `TerminateWindow`/`InitWindow`.)
- [ ] **USB reconnect (replug/EOF) re-binds**: pull and re-plug the cable (or host disconnect → auto-reconnect) → new session binds the surface within the 10s window. (`CURRENT_WINDOW` re-seed.)
- [ ] **`rustscreen stop` (Bye) closes the app cleanly**: host sends Bye → app `finishAndRemoveTask` + process kill; next `start` is a clean cold launch.
- [ ] **Glass-to-glass latency unchanged**: read the on-device latency report; p50 still ~33–43 ms (this code is off the hot path — any regression is a bug to investigate, not an expected cost).
- [ ] **Android Auto still works**: with RustScreen not in use, the car head unit behaves normally (foreground poll only runs while foregrounded).

- [ ] **Step 5: Record the result**

If all green, note the verification in the PR body. If any item fails, STOP and debug (likely
suspects: surface-event timing vs. `take_blocking`, or lib double-load) before merging.

---

## Self-Review

- **Spec coverage:** ✅ surface→Rust (Task 3), USB stays Kotlin (Task 4), manifest lib_name (Task 5), deps no-AndroidX (Task 1), invariants + reconnect gap (Task 3 `CURRENT_WINDOW`), device gate (Task 6). No spec requirement is unmapped.
- **Placeholders:** none — every code step shows the exact code.
- **Type consistency:** `from_ptr_acquire(*mut sys::ANativeWindow) -> Option<NativeWindow>` (Task 2) is called with `ndk_win.ptr().as_ptr()` (Task 3); `CURRENT_WINDOW: Mutex<Option<NativeWindow>>` stores `window.clone_acquire()` and reads `.as_ref()` then `.clone_acquire()` — consistent. `nativeInit`/`nativeOnUsbFd` names unchanged on both sides.
- **Scope:** single PR, one subsystem (the Android client entry point).

## Delivery

Single PR off `refactor/ndk-native-surface`: Tasks 1–5 implement, Task 6 verifies, then code review → fix bugs → push → open one PR.
