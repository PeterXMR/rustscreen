# Roadmap: RustScreen

## Overview

RustScreen de-risks first, then builds. The journey: scaffold the workspace (P0, done), then immediately attack the three scary unknowns as spikes — create a virtual display from Rust (P2, the keystone), move bytes over USB both ways (P1), capture+encode on macOS (P3), decode+present on the Pixel (P4) — before wiring them into a live sub-50 ms pipeline (P5), adding the touch back-channel (P6), and finally hardening, raising Rust purity, and packaging for distribution (P7, P8). The authoritative design source (full acceptance criteria, seam table, risk register) is `docs/superpowers/plans/2026-06-02-rustscreen-architecture-roadmap.md`; this ROADMAP.md is the execution tracker.

## Active Priorities — user-set 2026-06-05 (READ THIS FIRST; supersedes the PR-ladder ordering below)

The live pipeline already works (the Mac desktop renders on the phone; PRs #21–#24). What's left to make it a tool the user actually uses daily is, **in this exact priority order**, the three goals below. The PR ladder and Phase Details further down stay valid as reference detail, but **when choosing what to work on next, follow THIS list first.** Each goal is rephrased here in implementation terms so any session can pick it up cold.

### Priority 1 — `rustscreen` terminal app: install once, drive it with `start` / `stop`

**Goal (user's words, rephrased):** Install the host on the Mac somehow, then control it entirely from the terminal. `rustscreen start` runs the host locally as a background process; the moment the phone app is open, streaming begins on its own — no extra steps. `rustscreen stop` kills the local host.

**What that means in code:**
- One installable CLI binary named **`rustscreen`** (today the host is only `cargo run --bin p5_stream`, a *debug* build). Ship a **release** build (debug materially worsens encode/copy latency) onto `PATH` — via `cargo install --path`, a copied release binary, or later a Homebrew formula / signed `.app` (P8).
- **`rustscreen start`** — launch the capture→encode→send host as a background process and return the prompt. It must:
  - acquire / verify the macOS **Screen & System Audio Recording** grant and fail with a clear, actionable message if it's missing — never a silent black screen (folds in usability gap **U3**);
  - **wait for the phone**: if the AOA accessory isn't present yet, idle until it appears, then auto-start streaming (no manual trigger);
  - record its PID (or use a daemon/launch-agent) so `stop` can find and kill it.
- **`rustscreen stop`** — terminate the running host and tear down the virtual display cleanly so the Mac desktop reflows.
- Reframes old **ladder item 7** (was a menu-bar app) into a **terminal-first** interface, and absorbs **U2** (one-click launch) + **U3** (permission clarity).

**Done when:** from a fresh terminal on a clean machine, `rustscreen start` turns the phone into a second screen with no other steps, and `rustscreen stop` cleanly ends it.

### Priority 2 — Automatic reconnect (self-healing handshake)

**Goal (user's words, rephrased):** If either side goes away and comes back, it just reconnects — no restart dance. Close and reopen the phone app → reconnects. `rustscreen stop` then `start` again → reconnects. Unplug and replug the cable → reconnects. As long as the app is running on **both** Mac and phone, the link re-establishes itself automatically (the handshake).

**What that means in code:**
- `rustscreen start` (Priority 1) is **not** a one-shot that dies when the phone disappears — it is a **supervisor loop**: wait-for-accessory → run the handshake (`connect-hello` + `negotiate()`, both already built) → stream → on disconnect, tear that session down and **return to waiting**, re-arming for the next connect. The loop only exits on `rustscreen stop`.
- Detect disconnect on all three triggers: phone app closed (accessory fd closes / read error), host restarted (phone re-offers the accessory), cable replug (USB re-enumeration into accessory mode).
- Idempotent setup/teardown each cycle: drop the virtual display on disconnect, re-create on reconnect; no leaked threads, no wedged USB endpoint, no stale `needs_keyframe` state.
- This **is** old **ladder item 5** (hotplug / reconnect / clean teardown), promoted to second priority. The one-shot handshake already exists; the missing piece is the supervision loop + idempotent re-arm.

**Done when:** with `rustscreen start` left running, the user can close/reopen the phone app, restart the host, and replug the cable any number of times, and the screen comes back on its own within a couple of seconds each time.

### Priority 3 — "Native" feel: mouse moves on the Mac appear on the phone instantly

**Goal (user's words, rephrased):** Moving the mouse (or dragging a window) on the Mac should show on the phone with no perceptible lag — ideally it feels like one continuous screen.

**What that means in code:** keep driving glass-to-glass latency toward the **< 50 ms target** (practical floor ~43 ms; the Mac's locked-60 Hz `CGVirtualDisplay` and the phone's 60 Hz panel make sub-~30 ms unreachable on this hardware — **do not chase sub-floor numbers**). Already shipped (`48c1b05`): host encoder in-flight pacing + VideoToolbox low-latency rate control + phone input pacing, which took best-case to **~80 ms p50**. Remaining levers, highest-impact first:
1. **Non-blocking USB writes** (`aoa.rs:267`, `session.rs:595`): the per-frame **blocking** `flush()` costs ~22 ms where the wire itself needs only ~1–2 ms. Multiple in-flight bulk transfers (`set_num_transfers(3–4)`) + a non-waiting flush — preserving the **C-01** "no unflushed partial frame before a blocking read" invariant — is **~18 ms off glass-to-glass, the single biggest remaining win.**
2. **Real-time thread scheduling** for the capture / encode / USB threads → removes the 30–60 ms p95/p99 jitter spikes (the "occasionally feels laggy" cases), not the median.
3. **Phone decoder hints** (`KEY_PRIORITY=0`, `operating-rate=60` in `mediacodec.rs`) → one line each, cheap, A/B-test on device.
4. **Present-time pacing** (`releaseOutputBufferAtTime`) → marginal now (~0–3 ms), future-proofing.

Verify **every** latency change against the on-device per-stage report (the `run-on-device` skill prints capture/encode/send/decode/present + glass-to-glass), not by assumption. This is old **ladder item 6 step 2** plus the documented frontier TODOs.

**Done when:** measured glass-to-glass is consistently < 50 ms p50 with no visible spikes, and dragging a window / moving the cursor on the Mac looks real-time on the phone.

---

**Why this re-ordering vs. the ladder below:** the original ladder front-loaded latency (item 6) and feature breadth (cursor, HEVC, orientation). The user's call is that **usability comes first** — an installable, self-reconnecting tool (Priorities 1–2) is worth more than more features, and latency (Priority 3) is the finishing polish on top. Items not in the three priorities above (cursor visibility #3, live touch #4, orientation #8, HEVC #9, purity swap #10) are **deferred behind them**; HEVC (#9) in particular is *anti*-latency (higher encode cost on Tensor, no decode win) and should not be picked up while Priority 3 is open.

### Implementation backlog — concrete sub-tasks (single source of truth)

The specifics below came from two research sweeps (latency-frontier + whole-UX). Those separate TODO docs were **deleted on 2026-06-05** and their actionable content folded here so this ROADMAP is the one place to look. Grouped under the priority each serves. (The *why/findings* behind the latency work still live in `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`, kept as a reference record — not a to-do list.)

**Priority 1 (CLI `start`/`stop`) sub-tasks**
- Ship a **release** build, not debug (debug worsens encode/copy latency).
- Persistent permission path: the Android `USB_ACCESSORY_ATTACHED` launch intent carries a once-ever grant — guide the user to tick "use by default" so plug-in is hands-free after the first grant.
- Cold-start trims: replace the fixed `sleep(500ms)` at `p5_stream.rs` setup with a poll of `SCShareableContent`; lower the ~10 s rendezvous to ~3 s (~350 ms faster cold start, no phantom-display hang).

**Priority 2 (auto-reconnect) sub-tasks**
- ⭐ **Host auto-reconnect loop:** wrap AOA bring-up → handshake → stream in a retry loop; **keep the virtual display alive across reconnects** so the Mac desktop doesn't reflow each cycle. Don't leak `nusb` handles per iteration.
- **`nusb` hotplug** instead of the 100 ms poll (`aoa.rs:176-220` → `nusb::watch_devices()`, IOKit-backed): instant re-detect, ~50 ms off each connect; subscribe before req53 to avoid a missed-event race; keep the poll as fallback.
- **Android foreground service** (`connectedDevice` type) holding the session + wake lock, so it survives app-switch / notification shade instead of going black.
- **Force-keyframe on (re)connect** + actually wire `Control::RequestKeyframe` (both ends ignore it today): clean image within ~16 ms on every connect instead of garbage until the next GOP. Shares mechanism with the latency keyframe path.

**Priority 3 (latency) — ranked levers** (detail in the Priority 3 section above): non-blocking USB writes (~18 ms) → real-time thread scheduling (p95 spikes) → phone decoder hints (`KEY_PRIORITY`, `operating-rate`) → present-time pacing. **The unifying bug across the whole pipeline:** an unbounded queue at any stage = pure latency on a lossless USB link — bound it to 1–2, drop-oldest (drop-to-keyframe for coded video), never block waiting to drain. **Ruled out, do NOT chase:** HEVC (anti-latency on Tensor), 90 Hz Pixel panel unlock (firmware-locked, brick risk, ~5 ms), >60 fps source (`CGVirtualDisplay` hard-locked to 60 Hz on Apple Silicon). Floor ≈ 43 ms; < 50 ms reachable, ~30 ms not.

**Deferred backlog — real work, but behind Priorities 1–3:**
- **Live touch (the read-only-monitor fix)** — the full touch chain exists and is CI-tested (`android-client/src/touch.rs` → `Frame::Touch` → `macos-host/src/touch.rs` FSM + `CgEventSink`), but nothing captures a finger or injects in a *live* session: `MainActivity.kt` has no `onTouchEvent`, and `p5_stream.rs`'s reader thread **discards** inbound `Frame::Touch`. Wiring the existing FSM turns the read-only monitor interactive. (Old ladder item 4.)
- Cursor visibility on phone (item 3); orientation / landscape lock (item 8); HEVC toggle (item 9 — anti-latency); NativeActivity purity swap (item 10 — hot-path rewrite risk, no user value); packaging / signing / notarization (P7/P8).
- **Power note (settles "fixture vs novelty"):** in AOA the Mac is the USB host and supplies 5 V / ~500 mA, so the phone trickle-charges (~break-even vs decode + screen-on draw) — lower idle brightness to swing net-positive. Verify with `adb shell dumpsys battery`.

## Phases

**Phase Numbering:** Phases use the source roadmap's P0–P8 labels (not 1–N). Spikes are marked 🔬.

**Execution Order (agreed, non-numeric):** P0 → **P2** → P1 → P3 → P4 → P5 → P6 → P7 → P8. P2 is front-loaded ahead of P1 because it is the keystone risk (R1): if creating a virtual display from Rust fails, the whole architecture changes, so it is attacked first.

- [x] **Phase P0: Workspace Scaffold & Cross-Compilation** - Cargo workspace, cross-compile, thin Kotlin shell, CI (COMPLETE — PR #1)
- [x] **Phase P2: Create a Virtual Display from Rust** 🔬 - Keystone risk R1 RETIRED; phantom display via private `CGVirtualDisplay` (COMPLETE — branch `feat/p2-virtual-display`)
- [x] **Phase P1: USB Byte Round-Trip** 🔬 - **COMPLETE.** Live byte-exact round-trip green on M1↔Pixel 6a over AOA (no sudo): 32B warm-up + 4× 1 MiB @ **103.0 Mbit/s**. **D1 = AOA** (NCM/TCP fallback unused); XPORT-01 met. Connect-hello handshake + 16 KiB read-buffer fixes closed the two live bugs
- [x] **Phase P3: Capture + Hardware-Encode on macOS** 🔬 - **COMPLETE.** Full virtual-display capture → VideoToolbox H.264 → playable `out.h264` PROVEN on M1 (all-objc2 stack). All 3 criteria met with measured numbers: ffplay plays `h264 High yuv420p 2400×1080`; SPS 12 B/PPS 4 B extracted+in-band; per-frame encode latency **avg 10.29 ms** (RealTime, no B-frames). Wave A merged (PR #4); capture half (PR #11); encoder on `feat/p3-encode`. ENC-01 met
- [~] **Phase P4: Decode + Present on the Pixel** 🔬 - **Wave A done** (cable-free, branch `feat/p4-decode-support`): `VideoDecoder` port + `DecodeSession` orchestrator (Frame→configure/decode) + `protocol::nal::to_annex_b_access_unit` AU-build/SPS-PPS-injection logic, all TDD (host-tested, no device). **Wave B DEFERRED** (hardware-blocked: needs Pixel 6a) — AMediaCodec decode-to-surface onto `ANativeWindow` + JNI surface plumbing + Kotlin `SurfaceView`
- [~] **Phase P5: Live End-to-End Pipeline + Latency** - Cable-free protocol slice merged (PR #5: `Frame` codec + `negotiate()` — criterion #3 logic). Remaining: wire P1–P4 live + measure glass-to-glass < 50 ms (blocked on P1/P4)
- [~] **Phase P6: Touch Back-Channel → macOS Injection** - BOTH ends' cable-free logic done: Mac side (`macos_host::touch`: normalized→global CG mapping (no Y-flip) + single-pointer FSM behind `PointerSink` + live `CgEventSink` CGEvent injector & `AXIsProcessTrusted` gate behind `--features live-inject` + `p6_inject` bin) AND Android side (`android_client::touch`: raw `MotionEvent` → normalized `TouchEvent`, the inverse mapping, host-tested + cross-compiles to arm64). Deferred (device): Kotlin `onTouchEvent`→JNI capture shim + live Touch-frame send over the session
- [ ] **Phase P7: Robustness, UX, Codec Options, Signing + Rust-Purity Upgrades** - Hotplug, HEVC, menu-bar, notarize, pure-Rust adapters
- [ ] **Phase P8: Packaging, Distribution, OSS Hygiene** - Notarized DMG + release `.apk`, README/LICENSE, clone-to-second-screen

## Delivery PR Ladder (functional → usable → shippable)

> **⚠️ Superseded ordering (2026-06-05):** the **Active Priorities** section near the top of this file is now the authoritative sequencing. This ladder remains accurate as a *catalogue* of the work items and their dependencies, but the *order* to do them in is Priority 1 (CLI `start`/`stop`, folds in items 7/U2/U3) → Priority 2 (auto-reconnect, = item 5) → Priority 3 (latency, = item 6 step 2). Items 3, 4, 8, 9, 10 are deferred behind those.

Once the spikes (P1–P4) are merged, the remaining work ships as a priority-ordered ladder
of PRs. Earlier = more essential to a working, usable product. Each PR names the **feature
a user gets** and the phase it draws from. Items marked ⭐NEW were added on user request
(2026-06-04); they are largely **verification + polish on capability that already exists**
(the cursor is already composited via `setShowsCursor(true)`; the virtual display already
presents as a named, arrangeable monitor per P2), surfaced only once the live pipeline runs.

| Item | GH PR | Title | Phase | Feature it brings | Depends on |
|---|---|---|---|---|---|
| 1 | **#21** (merged) + **#22** (merged) | **Live video pipeline** | P5 | The Mac desktop appears live on the phone — host streaming bin (#21) plus the Android USB→decoder rendezvous + keep-screen-awake (#22) that make it render on device | P4 decode (merged PR #20) |
| 2 | #23 (merged) | **⭐NEW Phone presents as a real, arrangeable external display** | P2→P5/P7 | The phone sits in System Settings ▸ Displays as a named monitor you arrange (choose which side); held alive for the whole session; stable identity so macOS remembers its position; HiDPI scaling option (D6); removed cleanly on disconnect | item 1; coordinate with the `cg-virtual-display` objc2 work (merged PR #18) — same crate. **Cable-free slice done** (branch `feat/p2-arrangeable-display`): `DisplayConfig` builder + `hidpi_modes()` (D6) + `arrangement_origin()` TDD'd in CI (10 tests); `with_config`/`arrange` (`CGConfigureDisplayOrigin`)/observable `Drop` adapter compiled (macOS-gated); `p5_stream` wired (HiDPI on, right of main); stable identity preserved via defaults. **Eyeball-deferred** (hands-on-Mac): confirm it appears named/arrangeable/HiDPI in System Settings + macOS remembers position across reconnect |
| 3 | #24 | **⭐NEW Mouse cursor visible on the external screen** | P5 | Your Mac cursor shows on the phone when you move it onto that display (carry `setShowsCursor(true)` into the live capture path; verify on device; handle HiDPI cursor scaling + cursor-only-update frames) | item 1 (rides directly on it) |
| 4 | #25 | **Live touch back-channel** | P6 | Tap/drag on the phone moves and clicks the Mac cursor (the inverse direction of item 3) | item 1 |
| 5 | #26 | **Hotplug / reconnect / clean teardown** | P7 | Survive cable pulls; phantom display vanishes on disconnect; auto-reconnect | item 1 (shares `session.rs`/`lib.rs` with item 4 — serialize) |
| 6 | **#24 (◀ STEP 1 DONE — measured 2026-06-05)** | **Latency proof + stream tuning** | P5 | **Step 1 — DONE (PR #24):** per-stage + glass-to-glass instrumentation (SNTP clock-sync ±111 µs + `Stats` frames + `PipelineLatency`). Measured baseline on device: **glass-to-glass ~470–585 ms p50** (USB transit 0.5 ms ✓ and render 1.6 ms ✓ are fine; the cost is **queue buildup** — host encode channel ~155–270 ms + phone decoder input ~280 ms — from sub-60 fps throughput, ~25–30 fps sustained). **Step 2 — TODO (next PR, the real <50 ms work):** bound both queues + drop-to-keyframe when behind + set a VideoToolbox `AverageBitRate` (currently uncapped, p5_stream.rs:357). Measure-before-tune confirmed: the bottleneck is bufferbloat, not any single slow stage. ✅ Black-screen **bug** resolved (distinct from U1): the installed APK was the no-`live-decode` **echo** build, which decoded nothing even with a window dragged onto the display; the live-decode `.so` was rebuilt + reinstalled. (This bug masked U1 entirely; with render working, U1's "drag a window" workaround now behaves as designed.) | item 1 |
| 7 | #28 | **Menu-bar app + onboarding** | P7/D5 | Launch from the menu bar; connect/disconnect, resolution picker, latency readout; permission prompts | item 1 (ideally item 4) |
| 8 | #29 | **Resolution / orientation / landscape lock** | P7 | Rotate the phone and the display follows; landscape lock; pick resolution | item 1/item 4 |
| 9 | #30 | **HEVC codec toggle** | P7 | Optional HEVC for better quality / lower bandwidth, H.264 fallback | item 1 |
| 10 | #31 | **NativeActivity purity swap** | P7 | No user-facing change; drops the Kotlin shell (Rust-source purity ~99%) | all Android items (1, 4, 8) merged — rewrites their files |
| 11 | #32 | **Signing, notarization & release** | P7/P8 | A new user can clone, install, plug in, and get a second screen following only the README | items 1–8 functional |

**PR-number anchor (updated 2026-06-04):** latest merged = **PR #23** (item 2 — arrangeable external display). **Reprioritization (2026-06-04, user request):** the next PR opened is **item 6 (Latency proof + stream tuning)**, so it takes the next GitHub number → **PR #24**. Items 3–5 are deferred behind it; their *projected* numbers shift up to **#25–#27** accordingly. The GH PR numbers for the remaining items are *projected* — they assume the items are opened in ladder order with nothing interleaved. GitHub assigns the real number at creation, so if other PRs land between, shift these accordingly. The **Item** column is the stable identifier; **Depends on** references ladder items as "item N" and real GitHub PRs as "#N".

**Milestones:** item 1 = "works as a screen." items 1–3 = a real, arrangeable second monitor you can
see your cursor on. items 1–4 = the full README promise (touch-capable live monitor). Through item 7 =
daily-usable app. Through item 11 = shippable to other people.

**Two usability gaps folded into the ladder** (not previously explicit in the architecture doc):
keep-the-phone-screen-awake (`FLAG_KEEP_SCREEN_ON`, fold into item 1) and landscape orientation lock (item 8).

## Usability backlog — surfaced on first real use (2026-06-04)

Running the app for real exposed gaps the de-risking ladder never caught, because every prior milestone was validated by people who *knew* to drag a window onto the new display. For a genuine first-run user these block "I can see it working":

- **U1 — "I launched it and the phone is just black."** ROOT CAUSE (not a bug): the phone is an **extended** desktop — a separate, empty screen placed to the *right* of main (`p5_stream.rs` creates the virtual display + captures it by its own `display_id`) — **not a mirror**. Until a window is moved onto it, it is correctly empty/black. **Immediate workaround: move the mouse off the right edge of the main screen, or drag a window right — it appears on the phone.** Proper fixes: (a) an optional **mirror / "verify" mode** that clones the main display so launch immediately shows your real desktop on the phone; and/or (b) explicit first-run guidance ("drag a window here →"). The mirror toggle is a small *new* capability; the guidance overlaps item 7 onboarding.
- **U2 — "I'm running a debug binary from a terminal."** No one-click launch exists yet (`cargo … p5_stream` in debug). Want a lightweight runnable — a release build + launch script now, en route to item 7's menu-bar app and item 11's packaged signed `.app`.
- **U3 — Screen-Recording permission clarity.** If the running process lacks the macOS **Screen & System Audio Recording** grant, capture yields black frames with only a log line (`p5_stream.rs:291`). Surface an actionable prompt instead of silent black (also part of item 7).

**Where these live:** U1's mirror toggle deserves its own small slice; U1 guidance + U2 + U3 feed **item 7** (menu-bar + onboarding) and **item 11** (packaging). **Open sequencing question:** these arguably outrank item 6 — measuring latency matters little until a picture reliably appears. Flagged for your call.

## Phase Details

### Phase P0: Workspace Scaffold & Cross-Compilation
**Goal**: A Cargo workspace builds and runs on both platforms — a macOS binary and an Android app (thin Kotlin shell over the Rust `cdylib`) — with CI green.
**Status**: COMPLETE — delivered as PR #1 on branch `feat/p0-workspace-scaffold`.
**Depends on**: Nothing (first phase)
**Requirements**: FOUND-01
**Success Criteria** (what must be TRUE):
  1. A blank app launches on the Pixel 6a and `adb logcat` shows "hello from Rust" (Kotlin shell, Rust core).
  2. The `macos-host` binary builds and runs on the M1 Mac.
  3. GitHub Actions CI builds the Mac binary, the Android `cdylib` via `cargo-ndk`, and the debug `.apk` — all green.
**Plans**: Done (PR #1)
**UI hint**: yes

### Phase P2: Create a Virtual Display from Rust 🔬
**Goal**: A macOS virtual display is created entirely from Rust via the private `CGVirtualDisplay` API, behind a safe `VirtualDisplay` port, with its `CGDirectDisplayID` obtainable for downstream capture. This is the keystone risk R1.
**Depends on**: Phase P0
**Requirements**: VDISP-01
**Success Criteria** (what must be TRUE):
  1. Running the spike creates a 2400×1080@60 display that appears in System Settings ▸ Displays and is arrangeable (D6).
  2. The phantom display is created entirely from Rust source (ObjC++ `.mm` shim behind the safe `VirtualDisplay::new` port — no Rust core logic exposed to the shim).
  3. `display_id()` returns a valid non-zero `CGDirectDisplayID` enumerable via `CGGetActiveDisplayList`.
**Type**: Spike — expand into a detailed TDD plan only after the spike succeeds. If `CGVirtualDisplay` proves unworkable, surface to the user immediately (DriverKit / mirroring fallbacks change the architecture).
**Plans**: TBD
**UI hint**: yes

### Phase P1: USB Byte Round-Trip 🔬
**Goal**: Bytes move reliably in both directions Mac↔Pixel over a single USB-C cable, with the transport mechanism (AOA vs network-over-USB) decided on measured throughput/reliability. Resolves D1.
**Depends on**: Phase P0 (independent of P2; sequenced after P2 by agreed order)
**Requirements**: XPORT-01
**Success Criteria** (what must be TRUE):
  1. 1 MB echoes correctly Mac→phone→Mac (verified byte-for-byte) over USB-C.
  2. The round-trip reproduces reliably across cable replug.
  3. Measured throughput is documented with ≥ ~200 Mbit/s headroom for 1080p H.264, and D1 (AOA vs NCM/TCP) is decided with recorded rationale.
**Type**: Spike (dual sub-spike: A = AOA via `nusb` + `jni` `UsbManager.openAccessory()`, lead; B = NCM/TCP fallback). Expand into a TDD plan after the spike succeeds.
**Plans:** 1 plan (Wave A cable-free TDD + Wave B hands-on, in P1-01-PLAN.md). D1 verdict + measured throughput recorded here on completion.
Plans:
- [x] P1-01-PLAN.md — **COMPLETE incl. Task B3 live hardware (2026-06-04).** Code (branch `feat/p1-usb-roundtrip`, merged PR #6/#7): `Transport` seam + echo/pattern/throughput/chunking + framing regression + Android `echo_loop` (TDD); `nusb 0.2.3` gated behind `live-usb`; AOA host (`aoa.rs` handshake 51/52/53 + reacquire + `AoaTransport`/`NcmTransport`) + `p1_echo` spike; Android accessory glue (`nativeOnUsbFd` JNI + `accessory_filter.xml` + manifest intent-filter + `openAccessory`→`detachFd`). **Task B3 (live, branch `feat/p1-connect-hello`):** byte-exact 32B warm-up + 4× 1 MiB round-trip @ **103.0 Mbit/s** on M1↔Pixel 6a; two live bugs fixed (connect-hello handshake + `AccessoryFdTransport` 16 KiB read buffer).

**D1 verdict: AOA (Android Open Accessory).** The AOA path delivers a reliable byte-exact bidirectional round-trip from Rust/macOS with **no sudo / no entitlement** (A2 resolved — handshake via device-level control transfers, only the driverless accessory interface is claimed). NCM/TCP remains a documented fallback but was not needed for viability.

**Measured throughput: ~103 Mbit/s** (round-trip echo, 4× 1 MiB). Note this is a *synchronous round-trip* figure (`p1_echo` waits for each full echo before sending the next — latency-bound, no transfer pipelining), so it **under-measures** the one-way, queued host→phone streaming the video path actually uses. It is below criterion #3's original "≥ ~200 Mbit/s headroom" aspiration but already clears a 1080p H.264 stream (~10–25 Mbit/s) by **4–8×**. If the live P5 pipeline ever proves bandwidth-bound, the levers are (a) pipelining/queuing bulk transfers and (b) the NCM/TCP fallback; revisit the 200 Mbit/s bar against a one-way streaming measurement then.

**Criteria status:** #1 ✅ (1 MiB byte-for-byte echoes — verified ×4); #2 ✅ (reproduces across replug — the runbook documents the replug→re-handshake reset); #3 ⚠️ partial (D1 decided + throughput documented ✅; the literal ≥200 Mbit/s-headroom bar is not met by the synchronous echo, see note above — sufficient for MVP video, re-evaluate one-way in P5).

### Phase P3: Capture + Hardware-Encode on macOS 🔬
**Goal**: The virtual display is captured and hardware-encoded to H.264, producing a playable file, with codec config and per-frame latency observable.
**Depends on**: Phase P2 (needs the virtual display's `CGDirectDisplayID`)
**Requirements**: ENC-01
**Success Criteria** (what must be TRUE):
  1. A `.h264` file captured from the virtual display plays back correctly in ffplay/VLC showing the virtual desktop.
  2. SPS/PPS (codec config) is extracted and logged for downstream decode.
  3. Per-frame encode latency is logged (realtime/low-latency config, no B-frames, zero-copy IOSurface).
**Type**: Spike. Wrap VideoToolbox/SCK behind own `Encoder`/`Capturer` traits (R3 churn); CGDisplayStream fallback. Expand into a TDD plan after the spike succeeds.
**Plans:** 1 plan (Wave A cable-free TDD + Wave B hands-on-Mac, in P3-01-PLAN.md)
Plans:
- [x] P3-01-PLAN.md — **COMPLETE.** Wave A (AVCC→Annex-B + capture-select, TDD, merged PR #4); Wave B (all-objc2 stack, proven on M1):
  - ✅ **B0** — objc2 family wired behind off-by-default `live-capture` feature; `p3_probe` proved the `CGVirtualDisplay` is visible to ScreenCaptureKit (FB17797423 does NOT bite). Commit `3e70c0e` (PR #11).
  - ✅ **B1** — `p3_capture` proved `SCStreamOutput` delivers frames (97 frames/2s on M1↔virtual display); objc2 `define_class!` delegate on an owned GCD queue sidesteps the run-loop footgun. Commit `8cec440` (PR #11 merged).
  - ✅ **B2/B3** — `p3_encode` (branch `feat/p3-encode`, commit `a884de5`): SCK 420v capture → `VTCompressionSession` H.264 (RealTime, no B-frames) → `block2` output-handler extracts AVCC `CMBlockBuffer` + SPS/PPS → Wave-A `avcc_to_annex_b` (in-band SPS/PPS before first IDR) → `out.h264`. **All 3 criteria met:** (#1) ffplay decodes `h264 High yuv420p 2400×1080`; (#2) SPS 12 B/PPS 4 B extracted+logged+in-band; (#3) per-frame encode latency **avg 10.29 / min 8.60 / max 53.66 ms** (max = first-frame IDR + warm-up). 57 frames/3s (~19 fps arrival) on a near-static desktop, but ~10 ms encode sustains ~100 fps → encode is not the bottleneck. **Encoder PR open.**
  - **Stack note:** capture/encode built on the madsmtm **objc2** family (not doom-fish `videotoolbox`, which is 3-weeks-old/experimental, nor FFmpeg) after a supply-chain + maturity review — canonical, zero-RUSTSEC, pure-Rust, cohesive end-to-end buffer types. `CGDisplayStream` fallback is moot: obsoleted in the macOS 15 SDK, and SCK sees the virtual display anyway.

**Note (D-capture, decided 2026-06-04):** capturing the virtual display REQUIRES the macOS **Screen & System Audio Recording** permission — there is no third-party bypass (DriverKit has no display family; the private `CGVirtualDisplay` delivers no frames to its creator; DisplayLink itself requires the grant). Documented in README; the shipped app will request it once for its own bundle.
**UI hint**: yes

### Phase P4: Decode + Present on the Pixel 🔬
**Goal**: Hardware-decoded H.264 renders on the Pixel screen via `ANativeWindow` using decode-to-surface, with no Java decode wrapper.
**Depends on**: Phase P3 (needs the captured `.h264` + SPS/PPS)
**Requirements**: DEC-01
**Success Criteria** (what must be TRUE):
  1. The recorded desktop video plays full-screen on the Pixel 6a.
  2. Decoding runs through a thin Rust `ndk-sys` `AMediaCodec` wrapper (no Java decode wrapper).
  3. Frames render decode-to-surface onto `ANativeWindow` with no CPU/GPU round-trip copy (D3).
**Type**: Spike. Expect to own the MediaCodec wrapper (R4 — `rust_mediacodec` stale). Expand into a TDD plan after the spike succeeds.
**Plans:** 1 plan (Wave A cable-free decode-support TDD, done; Wave B hardware decode-to-surface, deferred).
Plans:
- [~] P4 Wave A — **DONE (2026-06-04, branch `feat/p4-decode-support`).** Cable-free, host-tested decode-support logic toward DEC-01 criterion #2 (no device):
  - `android_client::decode` — the `VideoDecoder` PORT trait (`configure(codec, SPS/PPS)` + `decode(access_unit)`, both `Result`-returning so the Wave-B `AMediaCodec` adapter can surface codec/FFI errors without panicking across the boundary), mirroring `macos_host::encode::Encoder`. Plus the `DecodeSession` orchestrator: `Frame::VideoConfig` → split SPS/PPS via `nal::extract_codec_config` + `configure`; `Frame::Video` → build a decoder-ready AU + `decode`; tracks codec-config-seen / decoded / keyframe counts; recovers config from an **in-band keyframe** when no `VideoConfig` was sent first; rejects a `Video` before any usable config (`DecodeError::NoConfig`). A test-only `RecordingDecoder` records every configure/decode so the orchestration is proven in CI with no device.
  - `protocol::nal::to_annex_b_access_unit` (+ `has_in_band_config`, `AccessUnitError`) — REUSES `iter_nal_units`/`extract_codec_config`/`nal_unit_type`: turns a `Frame::Video` NAL payload into a self-describing Annex-B access unit, injecting out-of-band SPS/PPS ahead of a **bare** keyframe (and never duplicating when the payload already carries them in-band — P3's encoder does). Empty payload rejected. Boundary/edge TDD.
  - **No new runtime deps** — default `cargo build --workspace` pulls nothing new; no `ndk`/`ndk-sys`/JNI/FFI. The future `live-decode` `AMediaCodec` adapter (Wave B) drops in behind `VideoDecoder` (`#[cfg(target_os = "android")]`), documented in the `decode` module placeholder.
  - **Verification:** workspace 135→153 tests green (protocol +8, android-client +10); clippy `-D warnings` clean; fmt clean. `protocol` cross-compiles to `aarch64-linux-android`; the `android-client` cdylib cross-build is covered by CI (cargo-ndk supplies the NDK linker; the bare local toolchain only fails the final `.so` link step, not compilation).
- [ ] P4 Wave B — **DEFERRED (hardware-blocked: needs Pixel 6a).** Thin `ndk-sys` `AMediaCodec` adapter implementing `VideoDecoder` (configure with SPS/PPS as `csd-0`; queue each AU for decode-to-surface onto `ANativeWindow`, D3 — no copy), JNI surface plumbing, Kotlin `SurfaceView`. Test input ready: `out.h264` from P3 (in-band SPS/PPS self-configures the decoder).

**Criteria status (honest):** #1 ("plays full-screen on the Pixel") ⚠ hardware-deferred (needs the phone); #2 ("thin Rust `ndk-sys` `AMediaCodec` wrapper, no Java decode wrapper") 🟡 the cable-free feed/orchestration half is delivered (port + session + AU logic, TDD), the `ndk-sys` adapter itself is Wave B; #3 ("decode-to-surface onto `ANativeWindow`, no copy", D3) ⚠ hardware-deferred.
**UI hint**: yes

### Phase P5: Live End-to-End Pipeline + Latency
**Goal**: P1–P4 are wired live so the Pixel shows the Mac's extended desktop in real time, with glass-to-glass latency measured under 50 ms.
**Depends on**: Phase P4 (and P1 transport, P2 display, P3 encode)
**Requirements**: PIPE-01
**Success Criteria** (what must be TRUE):
  1. A live extended desktop is visible on the Pixel, updating in real time. ✅ **MET (2026-06-05)** — confirmed on device after fixing a black-screen bug (the installed APK was the no-`live-decode` echo build, decoding nothing; rebuilt + reinstalled with `--features live-decode`). Distinct from U1 (expected empty extended desktop).
  2. Measured glass-to-glass latency is < 50 ms, with per-stage timings logged and the §2 budget revised with real numbers. ⚠️ **PARTIAL (2026-06-05, much improved):** instrumentation shipped + per-stage/glass-to-glass logged + §2 budget revised (PR #24). After input-side pacing (phone `InputPacer` + Exynos vendor low-latency key) the dominant **phone `arrive→decode` collapsed ~500 ms → ~10 ms p50 (stable)** — root cause (MediaCodec input-queue bufferbloat) confirmed and SOLVED. Glass-to-glass best **~80 ms p50** (~6× better). Still NOT under 50 ms: the bottleneck moved upstream to the host **`capture→encode`** stage (31→105 ms, growing) — the same bug class one stage up (VideoToolbox fed every frame async with no in-flight bound). Closure = **item I: host-side encoder pacing** (bound VT in-flight + freshest-wins drop at capture), mirroring the phone fix. See `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md` §6.
  3. The protocol frame codec round-trips (TDD-verified) and `VideoConfig` (SPS/PPS) is sent on connect and on each keyframe; handshake/resolution negotiation succeeds.
**Type**: Build (TDD for protocol framing + handshake negotiation).
**Plans:** 1 plan for the cable-free slice (criterion #3 logic only; criteria #1/#2 + live wiring are cable/device-blocked, deferred). In P5-01-PLAN.md.
Plans:
- [ ] P5-01-PLAN.md — `protocol::messages` Frame codec layered on `framing` + pure `negotiate()` (TDD, CI-green) behind one serde/postcard dep-gate checkpoint. Live transport / send-on-connect / decode loop / latency harness / touch deferred.
**UI hint**: yes

### Phase P6: Touch Back-Channel → macOS Injection
**Goal**: Touch on the Pixel drives the macOS cursor on the virtual display via injected `CGEvent`s.
**Depends on**: Phase P5
**Requirements**: TOUCH-01
**Success Criteria** (what must be TRUE):
  1. Tapping on the Pixel moves the macOS cursor to the correct location on the virtual display.
  2. Dragging on the Pixel produces a click-and-drag on the Mac (mouseDown → move → mouseUp).
  3. Coordinate mapping (normalized → global CG coords) is TDD-verified; injection is gated on `AXIsProcessTrusted()` with an actionable Accessibility-permission prompt.
**Type**: Build (TDD for coordinate mapping). Single-pointer mouse emulation only (D4).
**Plans:** 1 plan for the cable-free slice (criterion #3's coordinate-mapping-TDD clause + criteria #1/#2 LOGIC only; real CGEvent injection + `AXIsProcessTrusted()` gating + Android `AInputEvent` capture are hands-on-Mac/device-blocked, deferred behind the `PointerSink` port). In P6-01-PLAN.md.
Plans:
- [x] P6-01-PLAN.md — **cable-free slice DONE (2026-06-03, branch `feat/p6-touch-mapping`).** `macos_host::touch` — pure `map_normalized_to_global` (NO Y-flip: Quartz global space + normalized touch are both top-left/y-down) with clamp-finite/reject-NaN sanitation, and a single-pointer touch→mouse `PointerStateMachine` (D4) emitting `PointerAction`s behind the `PointerSink` injection port (the D0 seam the `CGEvent` adapter implements). TDD, workspace 90→105 tests; clippy/fmt clean.
  - **+ macOS injection adapter (2026-06-04, same branch):** `CgEventSink` (first concrete `PointerSink`, via `core-graphics`) maps `Down/Move/Up` → `LeftMouseDown / MouseMoved|LeftMouseDragged / LeftMouseUp` posted to `CGEventTapLocation::HID`; `accessibility_trusted()` (`AXIsProcessTrusted()` FFI) gates construction (`InjectError::NotTrusted` — TOUCH-01 onboarding); `p6_inject` smoke bin drives a scripted tap+drag through the FSM into the live sink, sourcing the rect from `CGDisplay::main().bounds()`. All behind `--features live-inject` (off by default), mirroring `live-usb`; builds + clippy clean under default/live-usb/live-inject on macOS. Cable-free & phone-free (runs on the Mac). **Merged as PR #8.**
- [x] **Android-side touch normalization (2026-06-04, branch `feat/p6-android-touch-input`).** `android_client::touch` — pure `to_touch_event`/`normalize`/`phase_for_action`: converts a raw Android `MotionEvent` sample (masked action + pixel x/y + view w/h) into a normalized `protocol::messages::TouchEvent`. The exact INVERSE of the host's `map_normalized_to_global` (same top-left/y-down convention ⇒ no axis flip end-to-end); clamps out-of-range, collapses non-finite/degenerate inputs to finite `[0,1]`. TDD (android-client 3→12 host tests); clippy/fmt clean; cross-compiles to `aarch64-linux-android`. **Remaining for P6 (deferred):** the Kotlin `onTouchEvent`→JNI capture shim that feeds `to_touch_event`, and sending the resulting `Frame::Touch` over the live session — both wait on the live pipeline (P5) and on-device verification (phone).
**UI hint**: yes

### Phase P7: Robustness, UX, Codec Options, Signing + Rust-Purity Upgrades
**Goal**: The MVP becomes robust and shippable — survives hotplug, gains HEVC and a menu-bar app, is signed and notarized — and the simplest-now adapters are swapped for pure-Rust ones without touching core logic.
**Depends on**: Phase P6 (MVP pipeline proven first)
**Requirements**: ROBUST-01, PURITY-01
**Success Criteria** (what must be TRUE):
  1. The host survives 20 replug cycles: clean teardown, auto-reconnect, and the virtual display is removed on disconnect so the desktop reflows.
  2. HEVC works on the Pixel 6a behind a flag (falling back to H.264), and a menu-bar app (D5) exposes connect/disconnect, resolution, and latency readout.
  3. The host is code-signed with hardened runtime and notarized, launching without Gatekeeper warnings.
  4. Rust-purity adapter swaps land without changing core logic — Kotlin shell → NativeActivity, ObjC++ shim → pure `objc2`, capture Swift bridge → `objc2-screen-capture-kit` — and the Rust purity % metric is updated (~99% target).
**Type**: Build (each purity swap is an isolated adapter replacement behind an existing port).
**Plans**: TBD
**UI hint**: yes

### Phase P8: Packaging, Distribution, OSS Hygiene
**Goal**: Both halves are distributable and a new user can go from clone to a working second screen using only the README.
**Depends on**: Phase P7
**Requirements**: PKG-01
**Success Criteria** (what must be TRUE):
  1. macOS ships a signed, notarized `.app` in a DMG; Android ships a signed release `.apk` (`cargo apk build --release`).
  2. A new user can clone, build both halves, install, plug in a Pixel 6a, and get a second screen following only the README.
  3. Repo OSS hygiene is in place: README (architecture + honest "100% Rust source" statement + per-platform build/run), LICENSE (MIT/Apache-2.0), CONTRIBUTING, and CI badges.
**Type**: Build.
**Plans**: TBD

## Progress

**Execution Order:** P0 → P2 → P1 → P3 → P4 → P5 → P6 → P7 → P8 (P2 front-loaded ahead of P1 — keystone risk R1).

| Phase | Plans Complete | Status | Completed |
|-------|----------------|--------|-----------|
| P0. Workspace Scaffold | — (PR #1) | ✅ Complete (merged) | 2026-06-02 |
| P2. Virtual Display (R1 keystone) 🔬 | spike ✓ (PR #3) | ✅ Complete — R1 retired, phantom display via private `CGVirtualDisplay` (merged) | 2026-06-03 |
| P1. USB Byte Round-Trip 🔬 | 1/1 (PR #6, #7 merged + connect-hello PR) | ✅ **Complete** — live byte-exact round-trip on M1↔Pixel 6a over AOA, **no sudo** (A2 retired): 4× 1 MiB @ **103.0 Mbit/s**. **D1 = AOA**; XPORT-01 met (throughput is a synchronous-echo floor, see Phase P1 note) | 2026-06-04 (HW) |
| P3. Capture + Encode 🔬 | 1/1 (PR #4, #11 merged; encoder PR open) | ✅ **Complete** — full capture→VideoToolbox H.264→playable `out.h264` PROVEN on M1 (ffplay: `h264 High yuv420p 2400×1080`; SPS 12 B/PPS 4 B; encode avg 10.29 ms). All 3 criteria met; ENC-01 met; FB17797423 retired | 2026-06-04 |
| P4. Decode + Present 🔬 | Wave A done (branch `feat/p4-decode-support`) | 🟡 Cable-free Wave A done — `VideoDecoder` port + `DecodeSession` + `nal::to_annex_b_access_unit` (TDD, toward DEC-01 #2); Wave B (AMediaCodec decode-to-surface, JNI, Kotlin) deferred — needs Pixel 6a | 2026-06-04 (Wave A) |
| P5. Live Pipeline + Latency | 1 merged (PR #5) + item 6 step 1 done | 🟡 Criterion #1 ✅ (live desktop on device). Criterion #3 logic merged. **Latency instrumentation (PR #24, item 6 step 1) DONE + measured on device 2026-06-05:** SNTP clock-sync (±111 µs) + `Stats` frames + `PipelineLatency`; baseline **glass-to-glass ~470–585 ms p50** (criterion #2 PARTIAL — instrumented + §2 budget revised, but ~10–15× over the <50 ms bar; bufferbloat from sub-60 fps). Criterion #2 closure = item 6 step 2 (bound queues + drop-to-keyframe + bitrate cap, next PR). | 2026-06-05 (measured) |
| P6. Touch Injection | 0/1 (PR #8 merged + Android slice) | 🟡 Both ends' cable-free logic done — Mac side (`macos_host::touch`: mapping + FSM + `CgEventSink`/`AXIsProcessTrusted` behind `live-inject` + `p6_inject` bin, PR #8 merged) and Android side (`android_client::touch`: `MotionEvent`→normalized `TouchEvent`); Kotlin capture shim + live wiring deferred | 2026-06-04 (logic) |
| P7. Robustness + Purity | 0/TBD | Not started | - |
| P8. Packaging + Distribution | 0/TBD | Not started | - |

**Legend:** ✅ phase complete · 🟡 substantial slice landed (cable-free / de-risked), remainder hardware-blocked · ⚠ deferred (needs phone) · blank = not started.

**Cable-free progress:** **every Mac-only / pure-logic slice through P6 is now implemented and merged** (P0–P6 logic on `main`). The autonomous cable-free well is essentially dry — what remains needs a hands-on-Mac session or the phone, per the blockers below.

**🔌 Hardware now permanently connected (2026-06-05):** the M1 MacBook and Pixel 6a are connected for the whole dev cycle. **The "cable-free slice first, hardware-deferred" pattern is no longer a constraint** — device / hands-on-Mac work can be implemented and verified inline, and new PRs need **not** be split into cable-free vs. deferred halves. The previously "deferred (needs phone)" / "hands-on-Mac" items (P4 Wave B, P5 live wiring + latency, P6 live touch, item-2/item-3 eyeball checks) are now **directly executable**. **One standing caveat:** running the macOS capture (`p5_stream`) requires the **Screen & System Audio Recording** grant on the *launching* app — launch it from a terminal that holds the grant (a process launched from Claude.app cannot capture without that grant; this is the U3 usability item).

## Blockers & What's Needed Next

The critical path to a working MVP is ~~P3 (capture+encode)~~ → **P4 (decode) → P5 (live wiring)**. P3 is now DONE; the two remaining critical-path phases are **phone**-gated. Touch (P6) is a complete-on-both-ends side-channel waiting only on that pipeline.

| Item | What's left | Blocker — what's needed to proceed | Who |
|------|-------------|------------------------------------|-----|
| ~~**P3 Wave B**~~ | ✅ **DONE** (2026-06-04) — `p3_encode` proved full capture→VideoToolbox H.264→playable `out.h264` (ffplay `h264 High yuv420p 2400×1080`; SPS 12 B/PPS 4 B; encode avg 10.29 ms). FB17797423 retired; all 3 criteria met. Encoder PR open | — | — |
| ~~**P1 live echo**~~ | ✅ **DONE** (2026-06-04) — 4× 1 MiB byte-exact @ 103.0 Mbit/s; D1 = AOA; XPORT-01 met. See HARDWARE-FINDINGS.md | — | — |
| **P4 Decode + Present** (next on critical path) | **Wave B only**: `AMediaCodec` decode-to-surface onto `ANativeWindow` (thin `ndk-sys` wrapper implementing the Wave-A `VideoDecoder` port) + JNI surface plumbing + Kotlin `SurfaceView`. **Wave A (cable-free port + `DecodeSession` + AU-build logic) is DONE** on branch `feat/p4-decode-support` (TDD, host-tested) | **Phone** to decode/present. Test input is READY: `out.h264` from P3 (push via `adb`); SPS/PPS in-band so the decoder self-configures | **User+phone** |
| **P5 live pipeline** | Wire P1–P4 live; measure glass-to-glass < 50 ms | Depends on P1 + P3 + P4 (all hardware) | blocked |
| **P6 live touch** | Kotlin `onTouchEvent`→JNI shim feeding `android_client::touch::to_touch_event`; send `Frame::Touch`; host calls `CgEventSink` on the **main thread** | Depends on the live session (P5) + phone | blocked |
| **P7 / P8** | Robustness, HEVC, menu-bar, signing/notarize, Rust-purity swaps, packaging | Depends on a proven MVP pipeline (P6) | blocked |

**Recommended next action:** **P4 (decode on the Pixel)** — now the sole critical-path blocker. The test input is ready (`out.h264`, with in-band SPS/PPS so a decoder self-configures). This needs the phone: a thin `ndk-sys` `AMediaCodec` decode-to-surface spike rendering `out.h264` full-screen on the Pixel 6a. I can write the cable-free `cdylib` logic + JNI surface plumbing autonomously; the on-device run (decode + eyeball) needs you + phone. After P4, P5 wires P1+P3+P4 live for the <50 ms measurement (the encode side already has ~40 ms of headroom — encode is ~10 ms).
