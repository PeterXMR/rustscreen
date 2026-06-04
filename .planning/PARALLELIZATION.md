# RustScreen — Parallelization Plan (finish-fast, multi-session)

Goal: run several PRs at once in separate Claude Code sessions **without any two PRs
editing the same file before they merge**. Logical independence isn't enough — a PR is
only parallel-safe if it *owns a disjoint set of files*. This doc is the file-ownership
contract. Each session reads it first.

Golden rule: **a session edits only the files its track OWNS. If it needs to touch a
contested file, it waits for the wave that owns it.**

---

## Dependency graph (the long pole)

```
        ┌─ critical video chain (strictly sequential — each must be proven before next)
        │
 C1 ─► C2 ─►┬─► C3   (latency)
(P4B    (P5 │
 decode) wire)└─► C4 (P6 live touch)
        │
        └────────── parallel-safe satellites (no dep on the live pipeline) ──────────┐
                                                                                       │
   P1 (cg-vdisplay → pure objc2)   P2 (packaging + docs)   P3 (menu-bar scaffold)
```

C1→C2→C3/C4 is the long pole and **cannot be parallelized internally** — C2 needs C1's
decode proven on the phone; C3/C4 need C2's live session. The satellites P1/P2/P3 have
**zero overlap** with the chain and run from day one.

---

## WAVE 1 — open these NOW, in parallel (4 sessions, none blocks another)

| Track | PR scope | OWNS (only files it may edit) | MUST NOT touch | HW |
|---|---|---|---|---|
| **C1** | **P4 Wave B: AMediaCodec decode-to-surface.** `ndk-sys` `AMediaCodec` adapter behind the existing `VideoDecoder` port; `nativeOnSurface(Surface)` JNI; `SurfaceView`→`ANativeWindow` handoff. Test input `out.h264` (self-configures via in-band SPS/PPS). | `crates/android-client/src/decode.rs` (add `#[cfg(target_os="android")]` adapter behind a new `live-decode` feature), `crates/android-client/src/lib.rs` (**add** `nativeOnSurface` only), `crates/android-client/Cargo.toml`, `android/.../MainActivity.kt` (Surface handoff) | `session.rs`, host code, protocol enums | phone |
| **P1** | **P7: `cg-virtual-display` shim → pure objc2.** Replace `shim.mm` + `cc` build with `objc2` msg-sends to the private `CGVirtualDisplay` classes. Safe `VirtualDisplay::new` API stays byte-identical. | `crates/cg-virtual-display/**` ONLY (lib.rs, build.rs, Cargo.toml; delete `shim.mm`) | everything else | Mac |
| **P2** | **P8: packaging + docs scaffolding.** `.app` bundle + codesign/notarize config, `cargo-apk`/`cargo-ndk` release profile + signing, README build/run per platform, LICENSE/CONTRIBUTING polish, CI release job. (Final clone-to-second-screen *acceptance* waits for the pipeline; all *scaffolding* is independent now.) | `README.md`, `LICENSE`, `CONTRIBUTING.md`, `.github/**`, packaging scripts, root `Cargo.toml` `[package.metadata.*]`, `android/app/build.gradle*` (release signing) | any `src/*.rs` logic | — |
| **P3** | **P7: menu-bar app scaffold (D5).** New `menubar.rs` (tray-icon + objc2 status item) wired against the **existing** `run_send_session` API. Additive only. | new `crates/macos-host/src/menubar.rs`, `crates/macos-host/Cargo.toml` | `session.rs`, `main.rs` body (coordinate merge — see note) | Mac |

**Note on P3 vs C2:** both may want `main.rs` (currently 6 lines). Keep P3 entirely in a new
module behind a `menu-bar` feature and have `main.rs` call into it with one line; if C2
lands first, P3 rebases its one line. Low risk, but P3 is the only *amber* track — if you
want zero coordination, hold P3 for Wave 2.

---

## WAVE 2 — unlocked when **C1 merges**

| Track | PR scope | OWNS | Needs merged |
|---|---|---|---|
| **C2** | **P5 live wiring.** Host: drive `run_send_session` over the live AOA transport (`aoa.rs`/`AccessoryFdTransport`). Phone: replace the echo loop with the `DecodeSession` RX loop. | `macos-host/src/session.rs`, `macos-host/src/main.rs`, `android-client/src/lib.rs` (`nativeOnUsbFd` body → decode loop) | C1 |
| **H** | **P7 HEVC toggle.** `video/hevc` on encoder + the C1 decode adapter, behind a flag; `VideoCodec::Hevc` already reserved in protocol. | `encode_vt.rs`, `decode.rs` (HEVC arm), `protocol/messages.rs` (additive) | C1 (shares `decode.rs`) |

---

## WAVE 3 — unlocked when **C2 merges**

| Track | PR scope | OWNS | Needs |
|---|---|---|---|
| **C3** | **P5 latency harness.** ms-timer/QR on the virtual display, per-stage timings, **<50 ms glass-to-glass gate**; revise §2 budget with real numbers. | `macos-host/src/latency.rs` + a latency bin | C2 |
| **C4** | **P6 live touch.** Android `onTouchEvent`→`nativeOnTouch` JNI + `Frame::Touch` send; host touch-RX → existing `CgEventSink` injection. | `MainActivity.kt` (`onTouchEvent`), `android-client/src/lib.rs` (`nativeOnTouch`), `session.rs` (touch RX branch) | C2 |
| **R** | **P7 hotplug/reconnect.** Detect cable pull both ends, tear down + auto-reconnect, drop the virtual display on disconnect. | `session.rs`, `android-client/src/lib.rs` | C2 (shares both) |

C4 and R both edit `session.rs` + `lib.rs` → **serialize them** (one merges, the other rebases),
or assign both to the same session.

---

## WAVE 4 — last (rewrites the Android shell)

| Track | PR scope | Needs |
|---|---|---|
| **N** | **P7: Kotlin shell → NativeActivity.** Replace `MainActivity.kt` + JNI entry with `android-activity` native-activity; window from `app.native_window()`, USB fd via `jni`→`UsbManager`, touch via `AInputEvent`. `decode.rs`/`transport.rs`/`protocol` unchanged. | **all** Android work (C1, C2, C4) merged — it rewrites the files they all edit |

---

## Already done (do NOT redo)
- **Capture → `objc2-screen-capture-kit`** (the P7 "swap Swift bridge" item): P3 already
  built the all-objc2 (madsmtm) stack. Verify, then strike from P7.

## Contested-file index (who may edit, when)
- `android-client/src/lib.rs` — C1 (add `nativeOnSurface`) → C2 (RX loop) → C4 (`nativeOnTouch`) → N (rewrite). Never two at once.
- `android/.../MainActivity.kt` — C1 (Surface) → C4 (touch) → N (delete). Serialize.
- `macos-host/src/session.rs` — C2 → {C4, R serialized} . 
- `macos-host/src/main.rs` — C2 (+ P3's one line).
- `protocol/src/messages.rs` — additive only (H); `Hevc` already reserved.
- `crates/cg-virtual-display/**` — P1 alone, isolated forever.

## Recommended kickoff
Open **C1 + P1 + P2** today (3 conflict-free sessions). Add **P3** as a 4th if you accept
one rebased line. C1 is the critical unblock — prioritize the tester's time on it.
