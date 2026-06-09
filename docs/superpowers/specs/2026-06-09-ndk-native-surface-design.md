# NDK-native surface path — shrink the Kotlin shell

**Date:** 2026-06-09
**Status:** Approved (design)
**Branch:** `refactor/ndk-native-surface`

## Goal

Reduce the non-Rust footprint of the Android client by moving the **surface / window /
activity-lifecycle** half of `MainActivity.kt` into Rust, using the NDK's native-activity
path. The USB-accessory permission flow stays in Kotlin because it touches Java-only APIs
(`UsbManager`, `BroadcastReceiver`, `PendingIntent`) that have **no NDK equivalent**.

Outcome: `MainActivity` goes from ~304 lines (~120 effective) to ~70 effective lines — a
~40% Kotlin reduction — with **no UI/UX change and no latency change** (none of this code is
on the glass-to-glass hot path; it runs once at session setup).

### Explicit non-goals

- **Not** zero Kotlin. The USB-accessory permission flow remains in Kotlin by necessity.
- **Not** a Gradle removal. The existing Gradle build is kept (lowest risk).
- **Not** a change to the decode/transport/touch pipeline, the USB fd handoff, or the
  protocol. Those are untouched.

## Why this is safe to do (key architectural fact)

The surface is ingested through two **source-agnostic** internal Rust channels:

- `SURFACE_SLOT` (`rendezvous::WindowSlot`) — consume-once rendezvous that binds a *fresh*
  decode session via `take_blocking(10s)` (cold launch / genuine reconnect).
- `SURFACE_MAILBOX` (`rendezvous::SurfaceMailbox`) — the PR #36 live-swap channel that lets a
  *running* session `AMediaCodec_setOutputSurface` onto a recreated surface during app
  background→foreground, with `mark_lost()` on destroy. No reconnect, no teardown.

Today both channels are fed by JNI functions Kotlin calls (`nativeOnSurface` /
`nativeOnSurfaceDestroyed`). **The channels do not care where the `ANativeWindow` comes
from.** If a Rust `NativeActivity` event loop feeds the same two channels from
`SurfaceCreated` / `SurfaceDestroyed` events, every downstream behavior — including the
fragile PR #36 swap — is preserved by construction. Everything after the channel is
byte-identical.

## Approach (selected)

`android-activity` crate, **NativeActivity backend**, plus a Kotlin `NativeActivity`
subclass for the USB glue.

Rejected alternatives:
- **Hand-rolled `ANativeActivity` callbacks via raw `ndk`/`ndk-sys`** — more `unsafe` glue to
  own, no benefit over the standard crate.
- **GameActivity backend** — drags in AndroidX, breaking the app's current zero-AndroidX
  dependency profile (called out in `android/app/build.gradle.kts`). We have no input
  widgets/gamepad needs, so NativeActivity is the lean fit.

## Architecture

Both entry points live in the **same** `libandroid_client.so`:

1. **Rust `android_main` (NEW)** — android-activity's event loop. Replaces the two surface
   JNI functions:
   - `MainEvent::SurfaceCreated` → build `ANativeWindow` from the event's native window;
     `SURFACE_SLOT.put(window)` + `SURFACE_MAILBOX.deposit(window.clone_acquire())`
     (mirrors today's `nativeOnSurface`).
   - `MainEvent::SurfaceDestroyed` → `SURFACE_SLOT.clear()` + `SURFACE_MAILBOX.mark_lost()`
     (mirrors today's `nativeOnSurfaceDestroyed`).
   - Pumps events for the activity's lifetime so swaps keep flowing.
   - Hosts one-time init (logger / panic hook) currently in `nativeInit`, OR `nativeInit`
     is retained and called from Kotlin `onCreate` — decided in the plan to minimize churn.

2. **Kotlin `MainActivity : android.app.NativeActivity` (SHRUNK)** — keeps `onCreate` /
   `onResume` / `onPause` / `onNewIntent` / `onDestroy` **only** for:
   - the USB-accessory permission flow (`UsbManager`, permission `BroadcastReceiver`,
     `PendingIntent`),
   - the foreground accessory poll (Android Auto workaround),
   - the session-thread spawn + the existing `nativeOnUsbFd(fd)` call (UNCHANGED),
   - `FLAG_KEEP_SCREEN_ON`.

   Removes: `SurfaceView`, `SurfaceHolder.Callback`, `currentSurface`, the `nativeOnSurface`
   / `nativeOnSurfaceDestroyed` external declarations and calls, and `setContentView`
   (NativeActivity owns the window). `super.onCreate()` / `super.onResume()` etc. MUST be
   called so android-activity's glue runs.

## Data flow (downstream unchanged)

```
USB fd ─▶ nativeOnUsbFd (Kotlin session thread, blocking)   [UNTOUCHED]
            │
            ▼
        decode session ─▶ SURFACE_SLOT.take_blocking(10s)   initial bind
                       └▶ poll_surface() each loop           live swap (PR #36)

ANativeWindow now ORIGINATES from android_main (was: Kotlin nativeOnSurface)
```

## Files touched

| File | Change |
|---|---|
| `crates/android-client/Cargo.toml` | add `android-activity` (native-activity feature), `ndk`/`ndk-context` as needed |
| `crates/android-client/src/lib.rs` | add `android_main` event loop; delete `nativeOnSurface` + `nativeOnSurfaceDestroyed` JNI fns |
| `android/app/src/main/java/com/rustscreen/client/MainActivity.kt` | base class → `NativeActivity`; strip surface code (~120→~70) |
| `android/app/src/main/AndroidManifest.xml` | add `<meta-data android:name="android.app.lib_name" android:value="android_client"/>`; keep USB intent-filters & orientation |
| Gradle | no change |

## Invariants preserved

- `jni_guard` catch-unwind on `nativeOnUsbFd`.
- `sessionActive` atomic single-claim latch.
- `SURFACE_SLOT` consume-once semantics + destroy-before-take race guard (`clear()`).
- PR #36 background→foreground live `setOutputSurface` swap (via `SURFACE_MAILBOX`).
- `Control::Bye` → `finishAndRemoveTask()` + `killProcess` clean-close path.
- Zero AndroidX dependencies.
- No glass-to-glass latency change (off the hot path).

## Risks (device-verification items, not design blockers)

1. **Surface-event timing.** NativeActivity owns the window differently than a SurfaceView;
   the ordering of `SurfaceCreated` vs. the USB session's `take_blocking(10s)` must be
   confirmed on device (cold launch-by-plug, and re-attach).
2. **Foreground-service interaction.** Changing the Activity base class may interact with the
   app-background-kill foreground-service work; verify background→kill→relaunch behavior.
3. **Lib double-load.** NativeActivity loads the lib via `android.app.lib_name`; reconcile
   with any remaining `System.loadLibrary("android_client")` so the lib isn't loaded twice
   or the JNI symbols missed.

## Testing

- Rust unit tests (rendezvous / mailbox) stay green; add/keep coverage for the channel feeds.
- `cargo clippy` + `cargo build` for the android target (`--features live-decode`).
- **On-device gate (hardware connected):**
  - cold launch paints a clean IDR;
  - app background→foreground swaps with no reconnect (PR #36);
  - `rustscreen stop` (Bye) closes the app cleanly;
  - glass-to-glass latency unchanged vs. baseline.

## Delivery

Single PR off `refactor/ndk-native-surface`: implement → code review → fix bugs → commit →
push → open one PR.
