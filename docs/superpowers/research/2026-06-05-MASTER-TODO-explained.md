# RustScreen — Master Optimization TODO (with benefits explained)

> The decision-ready consolidation of all research (latency rounds 1–3 + whole-UX sweep).
> For **every** item: what changes, **what dimension improves**, and **what you'll actually
> see or feel** — plus expected numbers, effort, and risk. Deep detail lives in:
> `2026-06-05-latency-optimization-frontier-TODO.md` and `2026-06-05-whole-ux-optimization-roadmap.md`.

## How to read this

Each item is tagged with the dimension(s) it improves:

| Tag | Dimension | "Better" means… |
|---|---|---|
| ⏱️ | **Latency** | the camera-measured glass-to-glass number (ms) drops |
| 🤚 | **Responsiveness** | it *feels* faster to your hand, even if the video number is unchanged |
| 🔌 | **Reliability** | fewer hangs/black-screens/manual restarts |
| 🖼️ | **Quality** | sharper text, correct color |
| 🔋 | **Sustainability** | cooler phone, better battery, stable over hours |
| 🚀 | **Usability** | a thing you couldn't do before, now works |
| 🛠️ | **Foundation** | enables/unblocks other gains (no direct user effect alone) |

**Effort:** S = hours · M = a day · L = multi-day. **Risk:** Low/Med/High.

---

## Where the time goes today (the mental model)

After the phone fix already in PR #24, a frame's journey (Pixel 6a + M1, 2400×1080@60):

```
capture → encode → send → (USB) → arrive → decode → present
  8ms      105ms*    22ms    ~0.5ms    ~0      10ms     12-17ms
           ^^^^^^    ^^^^                              ^^^^^^^^
           GROWING   blocking                         1 vsync (hard floor)
```
`*` capture→encode is 31→105 ms p50 and **growing** (p95 405 ms) — the current dominant problem.

- **Hard floor ≈ 43 ms** (8.3 capture + 4 encode + 2 USB + 12 decode + 16.7 present). Two pieces
  are immovable on this hardware: one 60 Hz vsync (16.7 ms) and Tensor decode (~12 ms).
- **< 50 ms is reachable; ~30 ms is not** (would need a 120 Hz phone panel + 120 Hz capture source,
  both blocked — see the refresh-rate dead-end in the latency doc §5–6).
- **Today's best is ~80 ms p50 drifting to ~158 ms.** The whole gap to 50 ms is *removable
  queueing* in two host stages. That's what Tier 1 below attacks.

---

# TIER 1 — Biggest wins (latency + the things that make it usable at all)

### 1. Turn on VideoToolbox low-latency encode mode  ⏱️ 🛠️
- **What:** Pass `EnableLowLatencyRateControl` + set `MaxFrameDelayCount=1` when creating the
  encoder. Today the app passes `None` and relies on `RealTime` (just a hint).
- **What improves:** Latency — caps the encoder's *internal* frame buffering from the inside.
- **What you'll feel:** On its own, modest; its real job is to **make item 2 actually work** (the
  encoder stops secretly holding frames). Together with #2 it's half the fix for the growing lag.
- **Numbers:** 10–40 ms off `capture→encode` steady state. **Effort S · Risk Low** (must keep ABR,
  not CBR — they're incompatible).

### 2. Bound the host encoder queue (in-flight pacer) — *the* dominant latency fix  ⏱️
- **What:** Stop submitting every captured frame to VideoToolbox blindly. Track "frames submitted
  but not yet encoded"; if more than 1–2 are in flight, **drop the new one** (a live mirror wants
  the freshest frame). This is the exact mirror of the phone fix that already worked.
- **What improves:** Latency — directly attacks the stage that's growing to 105 ms / 405 ms p95.
- **What you'll feel:** This is the big one. Today, **drag a window and it rubber-bands further and
  further behind your gesture the longer you use it** (the bufferbloat grows). After this, the lag
  **stops growing and collapses to ~one or two frames, and stays there** — the picture tracks your
  actions in near-real-time indefinitely instead of degrading.
- **Numbers:** `capture→encode` p50 ~105 → ~10–20 ms; the 405 ms p95 tail disappears. Likely the
  single change that gets glass-to-glass from ~80–158 ms into the ~50 ms band. **Effort S–M · Risk
  Low** (the in-flight counter must decrement on every code path or the pacer wedges).

### 3. Keep the USB pipe full (multiple in-flight transfers, stop the per-frame waiting flush)  ⏱️
- **What:** The host currently writes one frame, then **waits for the whole transfer to finish**
  before grabbing the next frame (`flush()` after every frame), with only ~1 transfer in flight.
  Queue 3–4 transfers and make the frame-boundary flush non-waiting.
- **What improves:** Latency — the `encode→send` stage is ~22 ms of which only ~1–2 ms is actual
  wire time; the rest is the host *blocking* on each transfer.
- **What you'll feel:** Another ~18 ms shaved off everything — the whole image is consistently
  ~18 ms closer to live. Combined with #2, motion feels tight and immediate.
- **Numbers:** `encode→send` ~22 → ~2–4 ms. **Effort S–M · Risk Med** (don't break the
  flush-before-read rule; don't change the 16 KiB chunk size — it's load-bearing).

### 4. Wire up touch input (it's built but not connected)  🚀
- **What:** The Android app has no touch handler and the host *discards* incoming touch frames —
  so the whole tested touch→click pipeline is dormant. Connect it (Android `setOnTouchListener` →
  send `Frame::Touch`; host routes it to the existing FSM → CGEvent, on the **main thread**).
- **What improves:** Usability — turns a **read-only** display into an **interactive** one.
- **What you'll feel:** You can actually **tap, click, and drag on the Mac from the phone's
  touchscreen**. Right now you literally can't — touching the phone does nothing.
- **Numbers:** N/A (capability). **Effort M · Risk Med** (must inject on the main thread or macOS
  silently drops events; must map to the *virtual* display, not the main one).

### 5. Host auto-reconnect loop  🔌
- **What:** Today any cable replug, USB hiccup, or phone sleep/wake **kills the session and needs a
  manual `p5_stream` restart on the Mac**. Wrap bring-up in a retry loop; keep the virtual display
  alive across reconnects. (The phone is already reconnect-ready.)
- **What improves:** Reliability — the #1 daily-use friction.
- **What you'll feel:** Unplug and replug the cable and the monitor **comes back by itself in ~1–2
  s**, with your desktop layout intact — instead of going dead until you go restart the Mac program.
- **Numbers:** recovery: never → ~1–2 s automatic. **Effort M · Risk Low–Med.**

### 6. Fix the build profile (it's optimizing for size, not speed)  🛠️ ⏱️
- **What:** The release build uses `opt-level="z"` (smallest binary). Switch to `opt-level=3` +
  thin LTO + `target-cpu` (apple-m1 / cortex-a76); add a separate `dist` profile for the shipped
  app where size matters.
- **What improves:** Foundation + a little latency — and it **gates every benchmark**: measuring the
  other items on a size-optimized build under-reports the gains.
- **What you'll feel:** Slightly lower CPU cost and jitter on the encode/framing path; mostly this
  is "do it first so everything else measures true."
- **Numbers:** 20–40% fewer CPU cycles on the scalar hot path. **Effort S · Risk None.**

### 7. Local touch feedback / cursor (perceived instant)  🤚
- **What:** Draw the cursor (and a touch ripple) **on the phone, locally, the instant your finger
  moves** — decoupled from the video round-trip. This is how Parsec/RDP feel snappy.
- **What improves:** Responsiveness — the *felt* latency, independent of the video number.
- **What you'll feel:** The pointer moves **the moment you touch**, even though the underlying screen
  image is still ~50 ms behind. Interaction feels native instead of laggy. (The video catches up a
  beat later, which the eye forgives for a cursor.)
- **Numbers:** felt cursor latency ~10–20 ms regardless of the video floor. **Effort S (ripple)–M
  (cursor) · Risk Med** (start with non-predictive feedback so it can't be "wrong").

**After Tier 1:** glass-to-glass **stable in the ~45–50 ms band** (no more growing lag), the monitor
is **interactive and feels instant to the hand**, and it **self-heals on replug**. This is the jump
from "impressive demo" to "daily driver."

---

# TIER 2 — Quality, reliability, sustainability

### 8. Real-time thread priority  ⏱️ 🤚
- **What:** Mark the capture/encode/USB threads (Mac) and decode threads (phone) as high-priority
  (QoS user-interactive / SCHED_FIFO).
- **What improves:** Latency *tail* (p95/p99) — kills the occasional spike when a background thread
  preempts the pipeline.
- **What you'll feel:** Fewer random hitches/stutters. The average is already good after Tier 1; this
  makes it **consistently** good — no surprise 60 ms jumps. **Effort S–M · Risk Low.**

### 9. Crisp-text bundle: H.264 High profile + correct DPI + higher bitrate  🖼️
- **What:** (a) Set H.264 **High profile** (enables CABAC + 8×8 transform — never set today, default
  is Baseline/Main). (b) Fix the reported DPI — the code claims ~110 ppi for a ~429 ppi panel, so
  macOS picks soft scaling. (c) Raise bitrate to ~25–30 Mbit (the USB link is only ~7% used).
- **What improves:** Quality — specifically text sharpness, the #1 complaint for a *productivity*
  second screen. (4:4:4 chroma would fix color fringing but the Tensor decoder can't do it in
  hardware — so this is the realistic path.)
- **What you'll feel:** **Text stops looking soft/fuzzy and colored text stops fringing** — code and
  UI become crisp and correctly sized instead of slightly blurry. **Effort S each · Risk Low.**

### 10. Idle-frame skip  🔋 ⏱️
- **What:** A second monitor is static 90% of the time. Detect "screen didn't change" (the capture
  API already tells you) and **skip encoding/sending** — the phone just keeps showing the last frame.
- **What improves:** Sustainability (heat + battery) and indirectly latency (no thermal throttle).
- **What you'll feel:** The phone **runs cool and battery holds**, and the low-latency wins **don't
  silently erode after an hour of heat**. Encode/decode duty cycle drops from 100% to ~5–20% on a
  normal desktop. **Effort M · Risk Low–Med.**

### 11. Kill the crash/hang hazards  🔌
- **What:** (a) Recover the poisoned mutex in the encode handler (one panic currently cascades and
  kills the stream). (b) Add read timeouts to the connection *setup* phase so a frozen phone doesn't
  leave a phantom display and a hung host. (c) Make the phone's receive loop interruptible on abrupt
  unplug.
- **What improves:** Reliability — removes specific "it just froze / black screen, had to force-quit"
  failure modes.
- **What you'll feel:** Fewer mysterious freezes; failures turn into a clean auto-reconnect (with #5)
  instead of a wedged app. **Effort S–M · Risk Low.**

### 12. Two-finger scroll + right-click  🚀
- **What:** Add a scroll frame to the protocol (only single-touch exists today) → inject scroll
  wheel events; long-press → right-click.
- **What improves:** Usability — scrolling is the most common Mac interaction and currently
  impossible from the phone.
- **What you'll feel:** You can **two-finger scroll and right-click** from the phone — it goes from
  "move/click only" to actually usable for browsing/reading/work. **Effort M (scroll)/S (right-click)
  · Risk Med** (keep the wire format append-only).

### 13. Force-keyframe on connect + connection polish  🔌 ⏱️
- **What:** Force a fresh keyframe whenever a (re)connection happens; replace the 100 ms USB poll
  with hotplug events; trim the fixed 500 ms startup sleep.
- **What improves:** Reliability + a faster connect.
- **What you'll feel:** On (re)connect you get a **clean picture within one frame** instead of a
  moment of garbage/black; plug-in goes live ~0.4–0.7 s sooner. **Effort S–M · Risk Low.**

**After Tier 2:** consistently smooth (no tail spikes), **crisp readable text**, **cool and
battery-friendly over a full day**, **scroll + right-click**, and far fewer freezes.

---

# TIER 3 — Depth & polish

### 14. Long GOP + on-demand keyframes  ⏱️
- **What:** Stop sending a full keyframe every second (a big periodic frame); send keyframes only
  when needed (startup, a drop, or a phone request). On a lossless USB link periodic keyframes are
  pure overhead.
- **What you'll feel:** Removes a small ~once-per-second latency/bandwidth "bump." Smoother p95.
  **Effort M · Risk Med.** (Note: VideoToolbox has no intra-refresh API — on-demand is the way.)

### 15. Latency-tail polish: DataRateLimits, timestamp frame release, verify decoder settings  ⏱️
- **What:** Clamp peak frame size (smooths the keyframe USB spike); use the timestamp form of the
  phone's frame-release; verify the stream uses `poc_type=2` and binds the hardware decoder.
- **What you'll feel:** Smaller, rarer hitches; a few ms off decode in the best case. **Effort S
  each · Risk Low.**

### 16. Foreground service + thermal-aware adaptation  🔌 🔋
- **What:** Run the phone session as a foreground service (survives app-switch); have the phone
  report thermal/battery state so the host can gracefully drop to 30 fps under heat before it throttles.
- **What you'll feel:** The monitor **doesn't go black when you switch apps on the phone**, and
  **stays smooth under thermal stress** instead of degrading. **Effort M–L · Risk Med.**

### 17. Architecture & micro-perf: watchdog supervisor, allocation cleanups, PGO, best-of-N clock  🛠️ ⏱️
- **What:** A small **supervisor process** that restarts the streamer if it ever crashes (this is the
  *right* slice of your "chain of sub-programs" idea — the video hot path stays in one process for
  zero-copy speed, but a watchdog + a separate touch process give crash isolation). Plus: remove the
  one hot-path vtable dispatch and per-frame allocations, profile-guided optimization, and best-of-N
  clock sampling so the reported latency numbers are trustworthy.
- **What you'll feel:** Rock-solid uptime (auto-restart on the rare crash), marginally lower jitter,
  and accurate measurements. **Effort S–M each · Risk Low.**

### 18. Quality polish: color-range correctness, HiDPI mode review, decoder low-latency hints  🖼️
- **What you'll feel:** Correct blacks/whites/saturation ("looks like a real monitor"), the sharpest
  default scaling, and slightly steadier decode timing. **Effort S each · Risk Low.**

---

## What's deliberately NOT on the list (so you don't chase dead ends)

- **Running at 90 Hz** — the Pixel 6a panel is firmware-locked to 60 Hz (unlock = brick risk for
  ~5.5 ms) **and** the Mac's capture source (`CGVirtualDisplay`) is hard-locked to 60 Hz on Apple
  Silicon (confirmed unbypassable — no DriverKit display class, Apple's own Sidecar is capped too).
  The refresh rate is a genuine wall.
- **USB 3 / isochronous / multi-endpoint** — AOA is bulk-only USB 2.0; the link is only ~7% used,
  so it's latency-bound, not bandwidth-bound. No speed there.
- **4:4:4 color, HEVC, faster codec/engine** — the Tensor decoder and VideoToolbox are
  fixed-function ASICs already at their floor; no faster path exists on this hardware.
- **SIMD, tokio, zero-copy framing rewrites** — they don't move mean latency for this workload
  (CPU work is microseconds); cleanliness only.

---

## The bottom line — what the app becomes, tier by tier

- **Today:** read-only monitor; lag grows the longer you use it (~80→158 ms); dies on replug.
- **After Tier 1:** interactive (touch works), feels instant to the hand (local cursor), stable
  ~45–50 ms that *doesn't drift*, self-heals on replug. — *the daily-driver jump.*
- **After Tier 2:** consistently smooth (no spikes), crisp text, cool & battery-friendly all day,
  scroll + right-click, far fewer freezes.
- **After Tier 3:** rock-solid uptime, smooth under heat, last-mile latency/quality polish.

**Suggested first move:** do **#6 (build profile)** + **#2 (host pacer)** + **#3 (USB transfers)**
together for the measured-latency leap, then **#4 (wire touch)** + **#7 (local cursor)** for the
usability/feel leap. Those five turn the demo into something you'd actually use every day.
