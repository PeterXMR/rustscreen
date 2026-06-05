# RustScreen — Whole-UX Optimization Roadmap

> Companion to `2026-06-05-latency-optimization-frontier-TODO.md` (which owns glass-to-glass
> latency). This doc covers **everything else that makes the app feel best/fast**: connection,
> input, reliability, quality, power/thermal, architecture, robustness, and hardware limits.
> Produced 2026-06-05 from a 5-agent sweep (architect + connection + input + quality/power +
> robustness/hacks). Latency items are referenced, not repeated.

## 0. The two findings that reframe the product

1. **The phone CHARGES over USB in AOA mode — it does not drain.** In accessory mode the USB
   roles invert: the **Mac is the host** and AOA *requires* the accessory (Mac) to supply 5 V /
   500 mA. So the Pixel trickle-charges from the Mac on the same cable. Catch: 500 mA (~2.5 W) in
   vs ~3–5 W draw (decode + screen-on + brightness) ⇒ **slow net charge or break-even, won't
   fast-charge.** Verify on the wired rig: `adb shell dumpsys battery` → `status` (2=charging) +
   `current_now` sign over 10 min. *This settles whether RustScreen is a desk fixture or a
   novelty — it's a fixture.* (Lower phone brightness when idle to swing net-positive — see §5.)
2. **Touch is implemented, tested, and NOT wired into the live pipeline.** The full chain exists
   (`android-client/src/touch.rs` → `Frame::Touch` → `macos-host/src/touch.rs` FSM + `CgEventSink`)
   and is CI-tested end-to-end — but **nothing captures a finger or injects a CGEvent in a live
   session**: `MainActivity.kt` has no `onTouchEvent`/`setOnTouchListener`, and `p5_stream.rs`'s
   reader thread *discards* inbound `Frame::Touch` (`Ok(_) => {}`). **Today it's a read-only
   monitor.** Wiring the existing FSM is the biggest usability unlock in this doc (§2.0).

---

## 1. Connection & startup UX — make "plug in → live" instant and self-healing

The phone is already reconnect-ready (latch releases, `WindowSlot` is re-deposit-safe). The host is not.

| # | Item | Impact | Effort | Risk |
|---|---|---|---|---|
| C1 ⭐ | **Host auto-reconnect loop** — wrap AOA bring-up→handshake→stream in a retry loop; keep the virtual display alive across reconnects (no desktop reflow) | Replug/sleep-wake recovers in ~1–2 s **automatically** instead of *never* (today needs manual `p5_stream` restart) | M | Low-Med (don't leak nusb handles per iter) |
| C2 | **nusb hotplug instead of 100 ms poll** (`aoa.rs:176-220` → `nusb::watch_devices()`, IOKit-backed) | Instant re-detect; ~50 ms off each connect; makes C1 crisp | S-M | Low (keep poll fallback; subscribe before req53 to avoid missed-event race) |
| C3 | **Verify hands-free permission path** — the `USB_ACCESSORY_ATTACHED` launch carries a *persistent* grant; the dialog is once-ever if the attach-intent path is taken (it is). Guide user to tick "use by default" | Removes the unbounded host block at device-hello; plug-in is hands-free after first grant | S | Low |
| C4 | **Foreground service** (`connectedDevice` type) holding the session + wake lock | Session survives app-switch/notification-shade instead of going black | M | Med (Android 14 FGS type, surface handoff) |
| C5 | **Force-keyframe on (re)connect** + wire `Control::RequestKeyframe` (both ends ignore it today) | Clean image within ~16 ms on every connect instead of garbage until next GOP | M | Low (shares mechanism with latency TODO #8) |
| C6 | **Bound the setup-phase reads + 500 ms startup sleep** — replace `sleep(500ms)` (`p5_stream.rs:270`) with a poll of `SCShareableContent`; lower 10 s rendezvous to ~3 s | ~350 ms faster cold start; no phantom-display hangs | S | Low |

**TTFF today:** cold ~1.5–2 s + manual tap + manual host run; **warm replug never auto-recovers.**
**After C1–C3,C6:** cold ~0.8–1.2 s + one-time tap; **warm replug ~1–2 s hands-free.**

---

## 2. Input / touch — make it interactive (currently it isn't) and make it feel instant

| # | Item | Impact | Effort | Risk |
|---|---|---|---|---|
| I0 ⭐⭐ | **Wire touch into the live session** — Android `setOnTouchListener`→JNI→`Frame::Touch`; host routes `Frame::Touch`→FSM→`CgEventSink` **on the main thread** (Quartz drops bg-thread events); source `DisplayRect` from `CGDisplayBounds(vdisplay.display_id())` (not main display) | Goes from **non-functional to functional**. The gate for everything else | M | Med (main-thread marshaling is the trap) |
| I1 ⭐ | **Local feedback (perceived zero-latency)** — touch-down ripple + local cursor sprite driven from local `MotionEvent`; later, local scroll preview with reconciliation | Makes touch feel ~10–20 ms regardless of the video return-leg (the real reason it feels slow). Biggest *felt* win | S (ripple) / M (cursor) / L (scroll preview) | Med (reconciliation snap-back if mistuned; start non-reconciled) |
| I2 ⭐ | **Two-finger scroll + right-click** — protocol-blocked today (only `Touch` frame exists); add append-only `Frame::Scroll{dx,dy}` (next tag = 9) → `CGEventCreateScrollWheelEvent`; long-press → `CGMouseButton::Right` | Scroll is the most common Mac interaction — biggest *capability* win | M (scroll) / S (right-click) | Med (append-only wire; pixel-precise scroll units) |
| I3 | **Priority-interleave touch ahead of video + immediate Down/Up send** (single AOA pipe HOL-blocks a touch behind a ~100 KB video AU) | Removes up-to-one-frame jitter from every touch | S | Low |
| I4 | **Coalesce Move events to ~60–90 Hz, keep last sample; never coalesce Down/Up** (Android batches via `getHistoricalX/Y`) | Prevents input-queue pile-up (the video bug, applied to input); smooth drags | S | Low |
| I5 | Multi-touch/pinch, keyboard (`Frame::Key` → `CGEventCreateKeyboardEvent`), momentum, palm rejection | Completeness | L | Med (keycode-map long tail) — defer to v2 |

**Coordinate mapping is already correct** (no-Y-flip, points-space — pinned to Apple docs). The one
latent bug: the live path must use the *virtual* display's id, not `CGDisplay::main()` (which
`p6_inject.rs:28` uses — fine for the demo, wrong for live).

---

## 3. Architecture & build — and the "chain of sub-programs" verdict

### 3.1 Chain of sub-programs — VERDICT: keep the hot path in-process
The repo *already* uses separate per-stage binaries (`p1/p3/p5/p6`) behind a `Transport` seam —
that gives the dev-time "independent, swappable, testable" benefit. Splitting the **live** pipeline
into piped OS processes is **not worth it**: for a sub-50 ms budget, even cheap IPC (pipe ~5–20 µs,
shm ~0.5–2 µs) is tolerable latency-wise, **but** the capture→encode handoff is currently zero-copy
via IOSurface; a process boundary forces either a ~3.7 MB/frame copy or IOSurface-handle-over-XPC
(real complexity + entitlements) for ~zero real benefit. Crash-isolation and per-stage tuning
(RT priority) are both achievable *within* one process.
- **Do instead:** a tiny **watchdog supervisor binary** (`std::process::Command::spawn` p5_stream,
  restart on exit) for crash-resilience at zero hot-path cost — and (optional) run **touch
  injection as a separate process** (low-rate, latency-insensitive) over a Unix socket for fault
  isolation of the CGEvent path. That's the right slice of "chain of programs."

### 3.2 Build profile — currently a performance regression
| # | Item | Impact | Effort |
|---|---|---|---|
| B1 ⭐ | **`opt-level = "z"` → `3`** (+ `lto="thin"`, `strip=false`); add a `[profile.dist]` (`inherits=release`, `opt-level="z"`, `strip=true`) for the shipped `.app` | 20–40% fewer CPU cycles on the scalar hot path (AVCC→AnnexB, framing, stats); affects *every* benchmark below — do first | S |
| B2 | **`target-cpu`**: `apple-m1` (host), `cortex-a76 +neon,+fp16,+dotprod` (android) in `.cargo/config.toml` | NEON/feature codegen | S |
| B3 | **PGO** for `p5_stream` (instrument→collect 60 s→optimize); commit `.profdata` | 5–15% fewer instructions on the hot loop | M (needs hardware) |
| B4 | BOLT post-link | another 3–8% i-cache | L — defer until CPU-path is the bottleneck |

### 3.3 Allocation / abstraction audit (jitter + cleanliness, mostly not mean latency)
- `CountingWrite<&mut dyn Write>` (`session.rs:232`) — the one vtable dispatch in the hot path →
  make it generic `CountingWrite<W: Write>` (zero-cost, lets LLVM inline). **S.**
- `Arc<Mutex<Option<(sps,pps,len)>>>` locked *every frame* (`p5_stream.rs:48,106`) → `OnceLock`
  (lock-free after first keyframe). Also fixes robustness A1. **S.**
- `LatencyStats.samples: Vec<u64>` grows unbounded (3.5 MB / 2 h) and `percentile()` sorts a clone
  (~5 ms) → fixed-size ring or `hdrhistogram`. **S.**
- `PipelineLatency` uses `VecDeque::remove(idx)` (O(n)) → `HashMap<pts_us, _>` (O(1)). **S.**
- session.rs (1675 lines) is **not** monolithic bloat — ~595 lines code, ~1080 tests. Leave it.

### 3.4 Startup/binary size
Binary cold-start is <50 ms — *not* the connection bottleneck (USB re-enum + the 500 ms sleep are;
see C6). The `dist` profile (B1) keeps the shipped binary small without taxing performance.

---

## 4. Display quality — crisp text without breaking latency

**4:4:4 / 4:2:2 are a dead end** — the Tensor decoder has no HW path (SW fallback blows latency +
thermal). You're correctly stuck at 4:2:0; attack text sharpness the other ways:

| # | Item | Impact | Effort |
|---|---|---|---|
| Q1 ⭐ | **H.264 High profile + CABAC** (`ProfileLevel = H264_High_AutoLevel`) — never set today; default is Baseline/Main (CAVLC, 4×4 only). High gives 8×8 transform + CABAC | Visibly crisper text/edges at the same bitrate; ~10–20% better efficiency. Tensor decodes High in HW | S |
| Q2 | **Fix reported DPI** — `lib.rs:121` hardcodes ~110 ppi; panel is ~429 ppi → macOS picks wrong scaling/soft text. Report a real ~135×61 mm size | Correct default text size + native 1:1 rendering | S |
| Q3 | **Raise bitrate to 25–30 Mbit** (link is ~7% utilized) — for *text* the latency doc's "lower to 12–15" is backwards; idle-skip (§5) means most frames aren't sent anyway. Keep ABR (not CBR) | Cleaner text edges | S |
| Q4 | **MediaCodec `KEY_LOW_LATENCY=1` + `KEY_PRIORITY=0`** at configure (not set today) | Tighter, more consistent decode timing | S |
| Q5 | **Color-range correctness** — `420v` is video-range; set 709 primaries/transfer/matrix tags end-to-end (or switch to `420f` full-range for desktop sRGB) | Correct blacks/whites/saturation ("looks like a real monitor") | S |
| Q6 | **HiDPI mode review** — `config.rs:102` registers native 2400×1080 + half-res; ensure native 1:1 is the default for sharp text/max real-estate | Sharp, space-efficient default | S (on-Mac eyeball) |

**Skip:** HDR/10-bit (6a isn't HDR), HEVC (latency doc rejects it).

---

## 5. Power, thermal & sustainability — keep the wins over hours

| # | Item | Impact | Effort |
|---|---|---|---|
| P1 ⭐ | **Idle-frame skip** — read `SCStreamFrameInfoStatus`; on `idle` (static desktop), skip encode+send entirely (phone holds last frame; heartbeat keeps disconnect detection) | Encode/decode duty cycle 100% → ~5–20% on a typical desktop. Biggest heat/battery win; mitigates thermal throttle that silently erodes latency over hours | M |
| P2 | **Thermal/battery-aware degradation** — phone reports `getCurrentThermalStatus()`/battery in the `Stats` back-channel; host ladder: cap fps (1/30) → reduce bitrate, *latency > fps > quality* | Latency/quality hold over a full day instead of degrading by lunch | M-L (needs hysteresis) |
| P3 | **Idle brightness dim** on the phone after N s of no new frames | Swings net power positive (§0.1) | S |
| P4 | Content-adaptive bitrate (static text = high bitrate cheap; video = capped) — largely subsumed by P1 + Q3 | Best of both | M |

---

## 6. Robustness — kill the crashes/hangs that ruin UX

The parsers are genuinely well-hardened (bounded alloc, canonical decode, `Option`-guarded NAL).
New issues found:

| # | Item | Severity | Where |
|---|---|---|---|
| R1 ⭐ | **Poisoned `params` mutex in the VT handler** can cascade-panic the stream (runs in a dispatch block, *not* behind the JNI catch_unwind). Use `lock().unwrap_or_else(|e| e.into_inner())` or hoist param-caching out of the per-frame path (also a perf win, §3.3) | MEDIUM | `p5_stream.rs:106` |
| R2 ⭐ | **No read timeout in setup phase** (handshake/clock-sync) — if the phone freezes after USB claim but before replying, the host wedges with a phantom display up. The heartbeat only covers the *streaming* loop | MEDIUM | `session.rs:146,212` |
| R3 | **Phone receive loop can't be interrupted** — abrupt unplug that leaves the fd blocked never returns; the Kotlin session latch never releases | MEDIUM | `lib.rs:71`, `session.rs:260` |
| R4 | **`read_frame` allocates full declared len (≤16 MiB) up front** — DoS amplifier on a hostile peer (bounded; low risk on point-to-point USB) | LOW | `framing.rs:65` |
| R5 | **Touch `nx,ny` decoded as raw f32 (NaN/range untrusted)** — host clamps today; add a regression test pinning the clamp (it's the *only* guard before `CGEventPost`) | LOW | `messages.rs:129` |
| R6 | `find_candidate` can hand the AOA handshake to the wrong Google-VID device | LOW | `aoa.rs:150` |

---

## 7. Hardware limits — what's already maxed (don't chase these)

Honest "no" list, so no effort is wasted:
- **USB 3 / SuperSpeed in accessory mode: unreachable.** AOA's `f_accessory` gadget is bulk-only,
  USB 2.0 HS (~480 Mbps, ~35 MB/s). At 20 Mbit you're ~7% utilized — **bandwidth-rich,
  latency-bound.** Headroom buys *quality* (Q3), not speed.
- **Isochronous transfers: not worth it & not reachable** — AOA exposes no iso endpoints (would need
  a custom non-shippable gadget); iso also drops error recovery you want. Multiple in-flight *bulk*
  transfers (latency TODO #3) captures the entire realistic win.
- **Multi-endpoint striping: unavailable** — AOA gives exactly 1 bulk IN + 1 bulk OUT.
- **CPU/GPU/NPU: maxed** — VideoToolbox (Apple Media Engine ASIC) and Tensor decode (c2.exynos
  fixed-function) are already the lowest-latency engines; the NPU can't decode H.264. The only
  decode lever is making the block *emit eagerly* (Exynos vendor key, latency doc), not a faster engine.
- **One real precision lever:** clock-sync is a single SNTP sample (±0.5–1 ms, susceptible to a
  one-off scheduling hiccup that skews the *whole session*). Take **best-of-N (5–10) ping/pongs,
  keep min-rtt** — ~10 lines, no new wire types, makes every reported latency number trustworthy.
  (PTP/hardware-timestamp: not worth it, no API surface here.)

**Security note:** the AOA link is unauthenticated; touch → real `CGEvent` clicks. The meaningful
control (macOS Accessibility permission, already gated via `AXIsProcessTrusted`) is in place, and
the host clamp is the sole chokepoint before `CGEventPost` (keep it, test it — R5). Acceptable for
the physical-USB threat model; if AOA is ever network-bridged it needs a handshake secret.

---

## 8. Master priority order (whole UX)

**Tier 1 — usability & daily-use unlocks (do these first; mostly not latency):**
1. **I0** — wire touch into the live session (it's currently read-only). *The* usability gate.
2. **C1** — host auto-reconnect loop (today every replug needs a manual restart).
3. **§0.1** — verify the phone charges (settles the existential UX question; pure measurement).
4. **B1** — fix the build profile (`opt-level=3`); it gates every perf measurement.
5. **I1** — local touch feedback / cursor (biggest *felt* responsiveness).

**Tier 2 — quality, robustness, sustainability:**
6. **Q1+Q2+Q3** — the "crisp text" bundle (High profile + DPI + bitrate), all S.
7. **P1** — idle-frame skip (biggest thermal/battery win).
8. **R1+R2+R3** — kill the poisoned-mutex crash and the setup/teardown hangs.
9. **I2** — two-finger scroll + right-click (biggest capability win).
10. **C2,C3,C5,C6** — connection polish + force-keyframe correctness.

**Tier 3 — depth & polish:**
11. **C4** foreground service; **P2** thermal-aware degradation; **Q4–Q6** decode/color polish.
12. **B2,B3** target-cpu + PGO; **§3.3** allocation cleanups; **§7** best-of-N clock sync.
13. **Watchdog supervisor process** (the right slice of "chain of sub-programs").
14. v2: multi-touch/pinch/keyboard (I5), BOLT (B4).

**The throughline:** latency (the other doc) is one axis; the *experience* is gated more by (a)
touch being unwired, (b) no auto-reconnect, and (c) thermal/charging sustainability than by shaving
more milliseconds. Do Tier 1 and the app goes from "impressive demo" to "daily driver."

## Sources
Architecture/build: [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html) ·
[rustc PGO](https://doc.rust-lang.org/rustc/profile-guided-optimization.html) · [IOSurface cross-process](https://developer.apple.com/documentation/iosurface).
Connection: [Android USB accessory + persistent grant](https://developer.android.com/develop/connectivity/usb/accessory) ·
[nusb hotplug](https://docs.rs/nusb/latest/nusb/fn.watch_devices.html) · [FGS connectedDevice](https://developer.android.com/about/versions/14/changes/fgs-types-required#connected-device).
Input: [CGEventCreateScrollWheelEvent](https://developer.apple.com/documentation/coregraphics/1541327-cgeventcreatescrollwheelevent) ·
[Android getHistoricalX/Y](https://developer.android.com/develop/ui/views/touch-and-input/gestures/movement).
Quality/power: [AOA protocol (host powers device 500 mA)](https://source.android.com/docs/core/interaction/accessories/protocol) ·
[H.264 High profile 8×8/CABAC](https://www.rgb.com/h264-profiles) · [Kodi MediaCodec 4:4:4 SW-fallback](https://github.com/xbmc/xbmc/issues/19083) ·
[SCFrameStatus idle](https://developer.apple.com/forums/thread/720228) · [AOSP low-latency MediaCodec](https://source.android.com/docs/core/media/low-latency-media).
Hardware/robustness: [AOA bulk-only / USB 2.0 HS](https://source.android.com/docs/core/interaction/accessories/protocol) ·
[USB iso vs bulk](https://www.beyondlogic.org/usbnutshell/usb4.shtml) · [nusb Endpoint](https://docs.rs/nusb/latest/nusb/struct.Endpoint.html).
