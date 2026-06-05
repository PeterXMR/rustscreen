# Glass-to-Glass Latency: Frontier Optimization Map & TODO

> Research artifact extending `2026-06-05-glass-to-glass-latency-root-cause.md` (on PR #24).
> Written 2026-06-05 after a 5-agent parallel research sweep (host pipeline, phone pipeline,
> USB/AOA transport, competitor teardown, Rust frontier + budget math). The root-cause doc
> solved the **phone** input-queue bug (~500 ms → ~10 ms). This doc maps everything left
> between the current **~80 ms p50** and the physical floor, and tells you how far Rust can
> realistically push this hardware.
>
> **One-line state of play:** the bottleneck has moved entirely to the **host**
> (`capture→encode` 31→105 ms p50 / p95 405 ms; `encode→send` ~22 ms). The same bufferbloat
> bug class that was on the phone is now in (1) VideoToolbox async submit and (2) the USB
> writer. Fix those two and < 50 ms is reached.

---

## 0. The unifying lesson

The *same* bug appears at every stage of the pipeline. Each fix is the same shape:

| Stage | Unbounded queue | Status |
|---|---|---|
| Phone MediaCodec **input** queue | feed every frame, no cap | ✅ FIXED (`InputPacer`, PR #24) |
| Host **VideoToolbox** async submit | submit every frame, no in-flight cap | ❌ item I (TODO #2) |
| Host **USB writer** | 1 in-flight transfer + `flush()` waits every frame | ❌ TODO #3 |
| Host **`mpsc` channel** (delegate→send loop) | `std::mpsc` is unbounded | ❌ TODO #5 |

**Rule:** bound the queue to 1–2, drop-oldest (or drop-to-keyframe for coded video), never
block waiting for the previous unit to drain. On a lossless USB link there is *nothing to
smooth*, so every buffer > 1 is pure latency.

---

## 1. The latency budget — what's physically possible on this hardware

Per-stage floors (Pixel 6a + M1, 2400×1080@60, 20 Mbit/s H.264):

| Stage | Theoretical floor | Current measured | Verdict |
|---|---|---|---|
| Capture sampling (½ frame @60 Hz grid) | ~8.3 ms | ~8 ms | **at floor** |
| Encode (VideoToolbox HW, RealTime) | ~4 ms | ~4–6 ms (when not queued) | **at floor** |
| USB transfer (avg ~42 KB AU) | ~1–2 ms | ~22 ms (blocking flush) | **huge slack — TODO #3** |
| Decode (Tensor/Exynos HW compute) | ~10–15 ms | ~10 ms (queue now drained) | **at floor** |
| Present (one 60 Hz vsync) | ~16.7 ms | ~12–17 ms | **at floor** |
| **Sum of irreducible floors** | **≈ 43 ms** | ~80–158 ms p50 | ~37 ms removable queueing |

### Verdict (resolved across two independent agents + sources)

- **The Pixel 6a panel is firmware-locked to 60 Hz.** (It's the budget "a" model; the
  Samsung panel can do 90 Hz but Google disabled it — only reachable via a risky vendor_boot
  mod, not shippable.) So **one vsync = 16.7 ms is immovable**, and "render at 90/120 Hz" is
  **off the table**. (A competitor-research agent initially claimed 90 Hz — that was wrong;
  corrected here.)
- **< 50 ms is physically achievable.** Hard floor ≈ 43 ms leaves ~7 ms headroom. Reaching
  it requires driving every *queue* (host encode, USB, channel) to ~zero — not making any
  at-floor stage "faster."
- **~30 ms is NOT achievable on this hardware.** Present (16.7) + decode (~12) ≈ 29 ms before
  capture/encode/transfer. You'd need a 120 Hz panel + 120 Hz capture + faster decode.
- **Existence proof the target is conservative:** Luna Display measures **11.3 ms avg
  glass-to-glass over USB** ([Astropad](https://astropad.com/blog/comparing-latency-luna-display/))
  — but on a 120 Hz iPad with a proprietary codec/dongle. The same-category lesson holds:
  the gap to floor is *buffering and pacing*, not raw pipeline horsepower.

**Where to spend effort:** ~100% on eliminating queueing (host encode + USB + channel),
~0% on the at-floor stages (capture, encode compute, decode compute, present).

---

## 2. Prioritized TODO

Ordered by `leverage ÷ effort`. Effort: S = hours, M = a day, L = multi-day.
Each item: what · where (file:line) · expected impact · effort · risk.

### TODO #1 — VideoToolbox low-latency mode + `MaxFrameDelayCount=1` ⭐ (S)
- **What:** Pass an encoder-spec dict with
  `kVTVideoEncoderSpecification_EnableLowLatencyRateControl = true` to
  `VTCompressionSessionCreate`, and set `kVTCompressionPropertyKey_MaxFrameDelayCount = 1`.
  This selects a dedicated low-delay HW pipeline with **one-in-one-out, no reordering** —
  it caps VT's *internal* buffering from the inside. Apple claims up to 100 ms reduction for
  720p30 ([WWDC21 10158](https://developer.apple.com/videos/play/wwdc2021/10158/)).
- **Where:** `p5_stream.rs:344-356` passes `None` for the encoder spec today. `RealTime=true`
  (`:367`) is only a hint and does **not** engage this pipeline. `MaxFrameDelayCount` never set.
- **Impact:** 10–40 ms off `capture→encode` steady-state; **prerequisite that makes TODO #2's
  pacer actually work** (encoder won't hold frames hostage).
- **Risk:** Low-med. H.264-only (fine). **Incompatible with `ConstantBitRate`** — so keep ABR,
  do NOT adopt CBR (see TODO #9). Verify `AverageBitRate` still applies in this mode (it does).
- **Source:** [EnableLowLatencyRateControl](https://developer.apple.com/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol),
  [MaxFrameDelayCount](https://developer.apple.com/documentation/videotoolbox/kvtcompressionpropertykey_maxframedelaycount).

### TODO #2 — Host in-flight encoder pacing (item I, mirror of `InputPacer`) ⭐ (S–M)
- **What:** Cap submitted-but-not-completed VideoToolbox encodes. Track `submitted − completed`
  via an `AtomicUsize`; before submitting an SCK frame, if in-flight ≥ N (1–2), **drop it**
  (a live mirror prefers the freshest frame) and force an IDR on the next admitted frame so the
  phone resyncs. This is the exact host analogue of `crates/android-client/src/pacing.rs`.
- **Where:** `p5_stream.rs:86-180` capture delegate submits **every** frame via async
  `encode_frame_with_output_handler` (`:167-176`) with no bound. `capture_us` stamped at `:113`,
  handler fires later (`:117-162`) → the gap *is* in-flight residence (monotonic growth to
  p95 405 ms = textbook bufferbloat).
- **Impact:** **The dominant fix.** Should collapse `capture→encode` p50 ~105 → ~10–20 ms and
  kill the 405 ms tail — directionally the ~50× the phone pacer achieved.
- **Risk:** Low. **Critical subtlety:** the in-flight counter must decrement on *every* handler
  path including the early-return error branches (`:121,124,136,141`) or it leaks and the pacer
  wedges shut. Keyframe-ness is only known *after* encode today, so drive resync via
  `kVTEncodeFrameOptionKey_ForceKeyFrame` rather than detecting it pre-submit.
- **Note:** add the in-flight counter *first* (it's the diagnostic gap that hid this — same as
  phone item H). See TODO #10.

### TODO #3 — USB: multiple in-flight transfers + stop the per-frame waiting flush ⭐ (S–M)
- **What:** The ~22 ms `encode→send` is **~1–2 ms of actual wire time wrapped in ~20 ms of
  synchronous blocking.** Two changes: (1) `EndpointWrite::set_num_transfers(3–4)` to keep the
  USB pipe full (default is ~1 in-flight → writes stall on the prior transfer); (2) stop calling
  `flush()` after **every** frame — `flush()` *submits and waits for all pending transfers to
  complete*, which serializes encode against transfer.
- **Where:** `aoa.rs:267` `EndpointWrite::new(...)` never calls `set_num_transfers`.
  `session.rs:556` `out.flush()?` after every `Frame::Video`. The measured span is
  `session.rs:463→473`.
- **Math:** 42 KB avg AU over USB 2.0 HS (~35 MB/s practical) = ~1.1 ms serialization; AOA is
  ~7% utilized (20 Mbit on a ~280–480 Mbps link) → **not** bandwidth-bound. The 20 ms is pure
  blocking.
- **Impact:** ~22 ms → ~2–4 ms for common frames; flatter p95 on IDR frames. ~18 ms off
  glass-to-glass on its own — **the single biggest transport win.**
- **Risk:** Med. (a) Preserve the **C-01 invariant**: the last partial chunk must still get
  pushed onto the wire (else the phone's `read_frame` deadlocks) — make the frame-boundary flush
  *non-waiting* (submit, don't `await` completion) rather than removing it. (b) **Do NOT change
  the 16 KiB `BULK_TRANSFER_SIZE`** (`protocol/src/lib.rs:20`) — it's load-bearing against a
  debugged truncation bug (host transfer > device read size = silent truncation + deadlock).
  Raising transfer *count* is safe; raising transfer *size* is not.
- **Source:** [nusb EndpointWrite](https://docs.rs/nusb/latest/nusb/io/struct.EndpointWrite.html),
  [nusb Endpoint queue](https://docs.rs/nusb/latest/nusb/struct.Endpoint.html).
- **Verify on-device:** read the negotiated bus speed via nusb to confirm AOA = USB 2.0 HS (the
  "no SuperSpeed in accessory mode" claim is empirically true but poorly documented).

### TODO #4 — Real-time thread scheduling (S–M) — the biggest *tail* (p95/p99) win
- **What:** macOS — promote the SCK delegate / encode callback / USB writer to QoS
  `USER_INTERACTIVE`, or for hard determinism `THREAD_TIME_CONSTRAINT_POLICY` via
  `thread_policy_set` (period 16.6 ms, computation ~5 ms, constraint ~10 ms). Android — the
  decode-feed/release threads to `THREAD_PRIORITY_URGENT_DISPLAY` (or `SCHED_FIFO`).
- **Where:** nothing sets priority today. SCK output runs on a plain
  `DispatchQueue::new("com.rustscreen.stream", None)` (`p5_stream.rs:365`); stream + stats
  threads are plain `std::thread::spawn` (`p5_stream.rs:448`).
- **Impact:** **Jitter / p95–p99, not mean.** Kills the occasional 30–60 ms glass-to-glass
  spike when a background thread preempts the encode callback or USB writer. On a 16.6 ms frame
  budget this is the highest tail-ROI item.
- **Effort:** macOS ~30 lines of `libc::thread_policy_set` (types are in `libc`) or one QoS
  attr; Android one-line `setThreadPriority` per thread. Start with QoS/URGENT_DISPLAY (low
  risk), escalate to TIME_CONSTRAINT/SCHED_FIFO only if tails persist.
- **Source:** [thread_policy_set](https://developer.apple.com/documentation/kernel/1418892-thread_policy_set),
  [thread_time_constraint_policy_data_t in libc](https://docs.rs/libc/latest/libc/type.thread_time_constraint_policy_data_t.html).

### TODO #5 — Bounded drop-oldest ring replacing the unbounded `mpsc` (S)
- **What:** Replace `std::sync::mpsc` between the encode callback and the send loop with a
  **bounded** SPSC ring (`rtrb` or `crossbeam::ArrayQueue`), capacity 2–4, **drop-oldest** on
  full.
- **Where:** `p5_stream.rs:361` `mpsc::channel::<EncodedFrame>()` (unbounded), sent at `:138`.
- **Impact:** Both mean and jitter — but the win is the **bounded/drop-oldest policy, not
  "lock-free."** An unbounded channel lets stale frames accumulate (the host-side mirror of the
  decoder bug); bounded guarantees the consumer always works on a near-fresh frame. (If you kept
  it effectively unbounded, mpsc→rtrb alone buys nothing.)
- **Risk:** Low. `rtrb` is wait-free, ~tens of ns/op.
- **Source:** [rtrb](https://crates.io/crates/rtrb).

### TODO #6 — Phone: timestamp `releaseOutputBufferAtTime(idx, now_ns)` (S)
- **What:** Replace the boolean `AMediaCodec_releaseOutputBuffer(codec, idx, true)` with
  `AMediaCodec_releaseOutputBufferAtTime(codec, idx, monotonic_now_ns)` (pass present-ASAP =
  `clock_gettime(CLOCK_MONOTONIC)` ns — do **not** predict a future vsync, that adds latency).
  Matches Moonlight's low-latency default.
- **Where:** `mediacodec.rs` `drain_output`, the `releaseOutputBuffer(..., true)` call.
- **Impact:** **Marginal now (~0–3 ms)** — the doc's "boolean form blocks until vsync" is only
  true when the Surface BufferQueue is stuffed, which the pacer has largely eliminated. Value is
  future-proofing against re-stuffing + matching the gold reference.
- **Risk:** Low. One-line FFI swap.
- **Source:** [Moonlight MediaCodecDecoderRenderer](https://github.com/moonlight-stream/moonlight-android/blob/master/app/src/main/java/com/limelight/binding/video/MediaCodecDecoderRenderer.java).

### TODO #7 — Verify SPS `pic_order_cnt_type=2` + confirm HW decoder bound (S, diagnostic)
- **What:** (a) Dump the live SPS and check `pic_order_cnt_type`. `=0` lets the decoder hold
  frames for reorder; `=2` forbids reordering (strict one-in/one-out, no DPB buffering). VT sets
  `AllowFrameReordering=false` but that does **not** guarantee the emitted SPS carries
  `poc_type=2`. (b) Log `AMediaCodec_getName` and assert it starts with `c2.exynos` (not the SW
  fallback `c2.android.avc.decoder`).
- **Where:** SPS parsed in `protocol::nal::sps_dimensions` → fed as `csd-0` in `mediacodec.rs`
  configure; decoder created via `createDecoderByType("video/avc")` (picks first match, no
  assertion).
- **Impact:** On Snapdragon `poc_type=0` causes up to 21× decode latency; **on Exynos the
  effect is reportedly insignificant** — so likely a few ms here, but it's the only lever on the
  decode floor itself and it's free if VT already emits `poc_type=2`. The decoder-name check is
  cheap insurance, not a speedup.
- **Effort:** S to verify; M to fix (one-time host-side SPS rewrite — exact Exp-Golomb re-encode
  — if VT won't emit `poc_type=2`; phone unchanged).
- **Source:** [ExoPlayer #8514](https://github.com/google/ExoPlayer/issues/8514).

### TODO #8 — Long/infinite GOP + on-demand keyframes (M)
- **What:** Stop emitting a full IDR every 60 frames (a 100–300 KB frame that spikes both encode
  and USB ≈ once/sec). Set `MaxKeyFrameInterval` very high and force an IDR via
  `kVTEncodeFrameOptionKey_ForceKeyFrame` only at (a) startup, (b) a TODO-#2 pacer drop, or
  (c) a phone `Control::RequestKeyframe`. On a lossless USB link periodic keyframes are **pure
  overhead** (WebRTC/Selkies use infinite GOP + on-demand; Moonlight uses reference-frame-
  invalidation).
- **Where:** `p5_stream.rs:404` `MaxKeyFrameInterval=60`; the protocol already has
  `Control::RequestKeyframe` but **neither end acts on it**; the inbound reader thread
  (`p5_stream.rs:515-550`) can add a `Control` arm to force the IDR.
- **Impact:** Removes the periodic ~1 Hz latency bubble (p95/max), smooths USB pacing, and bounds
  the cost of a pacer drop-to-keyframe resync.
- **Risk:** Med. Infinite GOP means a single corrupt frame has no recovery until the next forced
  IDR — so the back-channel becomes load-bearing for correctness. Acceptable on reliable USB.
- **Critical negative finding:** **VideoToolbox has NO public intra-refresh / GDR API** — the
  root-cause doc's "intra-refresh if VT supports it" resolves to *it doesn't*. Use on-demand IDR
  instead. (`EnableLTR` long-term-reference recovery is the more robust future option.)

### TODO #9 — `DataRateLimits` peak clamp (keep ABR, NOT CBR) (S)
- **What:** Add `kVTCompressionPropertyKey_DataRateLimits` (a sliding-window hard cap) to clamp
  peak frame size so the USB write doesn't spike on big frames. Keep `AverageBitRate`; consider
  lowering it to 12–15 Mbit if `encode→send` stays high (link has ~13× headroom).
- **Where:** `p5_stream.rs:399` (`AverageBitRate=20 Mbit`, no peak cap).
- **Impact:** Small, second-order (smooths IDR USB spike). **Do NOT switch to CBR** — it's
  mutually exclusive with TODO #1's low-latency mode, which is higher leverage. `DataRateLimits`
  *is* compatible with low-latency mode.
- **Source:** [DataRateLimits QA1958](https://developer.apple.com/library/archive/qa/qa1958/_index.html).

### TODO #10 — In-flight depth instrumentation, host + phone (S)
- **What:** Add `submitted − completed` (host VT) and `queued − presented` (phone codec) counters
  to the `Stats`/logs. Moonlight exposes exactly this ("frame queue delay", target < 16 ms).
- **Impact:** Closes the diagnostic gap that hid both bufferbloat bugs (you measured *residence*
  but not *depth*). Makes the next run *evidence* that TODO #1–#3 worked, not a guess.
- **Note:** percentile reporting (`LatencyStats::percentile`, nearest-rank) already exists and is
  correct — p50/p95/p99 are real. (Minor: it retains all samples for nearest-rank → unbounded
  memory on very long runs; switch to a histogram if that ever matters.)

### TODO #11 — `TCP_NODELAY` on the NCM/TCP fallback (S)
- **What:** `set_nodelay(true)` in `NcmTransport::connect`. Nagle is on by default and the
  framing writes header+payload separately — exactly what Nagle punishes (up to ~40 ms).
- **Where:** `aoa.rs:356` `TcpStream::connect(addr)`.
- **Impact:** 0 ms today (AOA bulk is the primary path); a latent ~40 ms footgun if NCM is ever
  used. Pure win, do it regardless.

---

## 3. Deferred / explicitly NOT worth doing

- **Threaded RX→decode→render assembly line on the phone (doc category C)** — L effort.
  Real (overlaps decode behind vsync, ~5–10 ms) but a bounded win now that the input backlog is
  dead. Threaded `AMediaCodec` is the classic source of `IllegalStateException`/recovery bugs.
  Defer until measurement shows decode still serializing against RX/present.
- **MediaCodec async callback mode** (`AMediaCodec_setAsyncNotifyCallback`) — **~0 latency**
  (ExoPlayer: improves throughput/jank, not end-to-end; Moonlight stays synchronous). Only
  consider for retry-loop *robustness*, not speed.
- **Zero-allocation hot path / `Bytes` / buffer pools** — the AVCC→AnnexB copy + framing concat
  allocate per frame, but a 42 KB memcpy at 60 fps is ~µs. **Does not move mean or p99 latency**
  — it's CPU/cleanliness only. Defer; if done, `write_vectored` for the framed send is the one
  worthwhile piece. (PR #24 correctly left these alone and went after the queue.)
- **SIMD / hand-vectorized memcpy** — skip. 42 KB is far below any SIMD threshold;
  `extend_from_slice` already lowers to optimized memcpy.
- **tokio / async runtime** — skip; would *add* jitter. A 3-thread realtime pipeline wants
  dedicated high-priority OS threads + bounded rings, not an async scheduler.
- **CBR rate control** — skip (mutually exclusive with TODO #1). Use ABR + `DataRateLimits`.
- **HEVC** — skip. Higher encode latency, no decode win on Tensor for H.264, USB size win
  irrelevant at ~7% link utilization. Low-latency mode is best-supported on H.264.
- **`queueDepth` / `minimumFrameInterval` tuning** — already at floor (queueDepth defaults to 3 =
  min; `minimumFrameInterval=1/60` already set). **Correction to root-cause doc:** SCK is *not*
  oversupplying >60 fps; the only cap that matters is in-flight VT (TODO #2).
- **90/120 Hz rendering** — not available (Pixel 6a firmware-locked to 60 Hz).
- **Lower-latency capture than SCK** — `CGDisplayStream` is obsoleted in macOS 15; SCK is the
  only path and is at floor.

---

## 4. Recommended order of attack

1. **TODO #10** (in-flight depth counters) — so the next run is evidence.
2. **TODO #1** (VT low-latency mode + `MaxFrameDelayCount=1`) — smallest change, caps VT buffering.
3. **TODO #2** (host in-flight encoder pacing) — the dominant `capture→encode` fix.
4. **TODO #3** (USB multiple transfers + non-waiting flush) — the dominant `encode→send` fix.
5. **Re-measure.** Expected: `capture→encode` p50 → ~10–20 ms, `encode→send` → ~2–4 ms,
   glass-to-glass into the **40–50 ms** band. If < 50 ms p50 → **P5 criterion #2 met.**
6. **TODO #4** (RT thread scheduling) — crush the p95/p99 tail to make < 50 ms *stable*.
7. **TODO #5, #6, #11** — cheap polish (bounded ring, timestamp release, TCP_NODELAY).
8. **TODO #8, #9** (long GOP + on-demand IDR, DataRateLimits) — remove the 1 Hz bubble.
9. **TODO #7** (verify `poc_type` / HW decoder) — last decode-side squeeze.

**Bottom line:** the floor is ~43 ms, < 50 ms is reachable, and the entire gap from today's
~80 ms is removable queueing in two host stages. TODO #1–#4 are all S/M effort and should land
the target; everything else is tail-tightening and polish.

## 5. Round 2 — "Can it run at 90 Hz / faster than the display?" (verdict)

Follow-up research round on whether we can beat the 60 Hz present floor. **Short answer: no
meaningful win is available on this hardware + architecture — for two independent reasons that
both lead to the same wall.** Two real but cheap protective wins came out of it (5.4, 5.5).

### 5.1 Unlock the Pixel 6a panel to 90 Hz — NO (brick-risk hack, not shippable)
- The 6a panel is 90 Hz-*capable* Samsung hardware but **firmware-locked to 60 Hz**.
  `Display.getSupportedModes()` returns 60-only; Dev Options / "Smooth Display" / `adb settings
  put system peak_refresh_rate 90` are all clamped to that list. An app **cannot** request a mode
  the HWC doesn't expose (`setFrameRate` is a hint, platform decides).
- A real community **90 Hz panel-driver mod** exists (TheLunarixus, open-sourced) but needs
  bootloader unlock + flashing a `vendor_boot`/system partition, and ships **green tint, Vsync
  tearing, proximity-sensor glitches, broken 60↔90 auto-switch**, breaks Play Integrity, and
  maintainers admit possible long-term panel damage. AndroidPolice: not for daily use.
- **Even if unlocked:** one vsync drops 16.7 → 11.1 ms, a **~5.5 ms** gain. Not worth the risk.
- **Verdict: HIGH risk, ~5.5 ms gain → don't.**

### 5.2 Run the pipeline >60 fps so the 60 Hz compositor latches a *fresher* frame — BLOCKED
- The mechanism is real: a 60 Hz panel still latches the **newest** completed buffer each vsync,
  so delivering at 90–120 fps lowers the average "frame age at scan-out" by ~half the (faster)
  source interval — a genuine **~4–6 ms average** glass-to-glass win (statistical, not a floor
  drop). The phone is already set up to exploit it (drop-stale-keep-newest in `drain_output`).
- **But it requires a >60 fps *source*, and there isn't one.** RustScreen does not capture the
  Mac's physical panel — it captures a **`CGVirtualDisplay`** it creates
  (`p5_stream.rs:282`; `cg-virtual-display/src/lib.rs:159`), and **`CGVirtualDisplay` is
  hard-locked to 60 Hz on Apple Silicon** (a framework limit confirmed by the BetterDisplay
  maintainer and multiple 120 Hz-hardware users — not the M1 Air panel, which is irrelevant here).
  SCK can only deliver "at most the source display's rate" = 60. So there is nothing to oversample.
- This also kills the "capture at 120, pace to 60" sweet spot (it was sound in theory; the source
  can't produce 120). **Expected gain on the current architecture: 0 ms.**
- The *only* way to get a >60 fps source is to abandon the virtual-display model and capture a
  real ProMotion (120 Hz) Mac panel — a different product, out of scope.
- **Verdict: correct idea, unreachable here → 0 ms. The 8.3 ms capture-sampling age and the
  16.7 ms present vsync are both genuine hard floors on this architecture.**

### 5.3 The honest ceiling
The present path is **one decode ⊕ one 60 Hz vsync (16.7 ms)** and the capture path carries an
**8.3 ms sampling age** — neither is removable without new hardware. Round 2 confirms the round-1
floor (~43 ms) is real and < 50 ms remains the right, achievable target; ~30 ms is not reachable.
Stop chasing the refresh rate; spend the effort on TODO #1–#4 (the host queueing bloat).

### 5.4 NEW — Protect the SurfaceView hardware-overlay path ⭐ (S, risk none — don't regress)
- A `SurfaceView` gets its own composition layer → SurfaceFlinger assigns it a **hardware overlay
  plane**, handing the decoded buffer straight to the display controller and **skipping GPU
  composition (~1 compositor frame saved)**. `TextureView`/`SurfaceTexture` is always
  GL-composited and adds **1–3 frames**.
- **Current state: already optimal** — full-screen `SurfaceView` (`MainActivity.kt:62-84`) →
  `ANativeWindow` → decode-to-Surface (`mediacodec.rs`). This is the single biggest present-side
  win and you already have it.
- **Action (a guard, not a change):** keep it full-screen + single-layer; never put translucent
  UI/chrome on top of the video (extra layers disqualify the overlay → forces GPU composition →
  +16–50 ms). Optionally confirm via `adb shell dumpsys SurfaceFlinger` that the layer shows as
  HWC overlay, not "GLES".

### 5.5 NEW — Verify the `420v` capture format matches VideoToolbox ingest (S, hidden mean-tax)
- SCK→VideoToolbox is **already zero-copy** (IOSurface-backed `CVPixelBuffer` passed straight to
  `EncodeFrame`, `p5_stream.rs:144-153`; the only CPU copy is the small compressed-output copy at
  `:241`, which is unavoidable and trivial). **But** if the configured pixel format (`420v`,
  `p5_stream.rs:333`) doesn't match what the encoder ingests, VT **silently inserts a
  pixel-format conversion** = an extra copy + real mean latency. `420v` (NV12 video-range) is the
  encoder-native choice and looks correct — **verify no conversion is happening** (this is
  insurance against a hidden tax, not a known bug).

### 5.6 Round-2 deeper Rust hacks — mostly skip (jitter-only or placebo)
- **`releaseOutputBufferAtTime(idx, monotonic_now_ns)`** — already TODO #6. Reaffirmed: ~0–2 ms
  now (pacer keeps the present queue shallow), matches Moonlight; cannot present *before* a vsync,
  so "land a stage earlier" is a placebo. Still worth doing as cheap future-proofing.
- **Spin-then-park** on the bounded-ring handoff (micro-spin a few µs before parking) — small
  **p99/jitter** win, S effort. Do it bounded (don't burn a core).
- **Android big-core affinity** (`sched_setaffinity` on the decode/release thread) — modest
  **jitter** win, after RT-priority (TODO #4) lands. macOS has no real pinning (only affinity
  *hints*) — use QoS instead.
- **Skip:** full 120 fps end-to-end (2× decode load re-introduces the input-queue bug),
  speculative/vsync-aligned encode submit (fragile, prediction error > savings over USB clock
  sync), `mlock`/pre-fault (no win without buffer pools), `setFrameRate(FIXED_SOURCE)` (no
  alternate mode to pick on a locked panel), front-buffer/latch-unsignaled (need to own a GL
  surface; inapplicable to decode-to-Surface).

### Round-2 net
The refresh-rate angle is a **dead end** (panel unlock = brick risk for 5.5 ms; pipeline >60 fps
blocked by the 60 Hz virtual-display source). The actionable output is small and cheap: **protect
the overlay path (5.4)** and **verify the pixel format (5.5)**; everything else is jitter-tier.
The real prize is unchanged — the host queueing bloat in TODO #1–#4.

## 6. Round 3 — Can we rewrite to beat the 60 Hz capture cap? (hacker mode)

Deep reverse-engineering round (4 agents: virtual-display internals, private-framework RE,
capture-side phase latency, alternative architectures). The question: can a rewrite — or a
separate component/driver — make the Mac capture faster than 60 Hz?

### 6.1 Verdict: BLOCKED for any virtual-display path (high confidence, multi-source)
You **cannot** get a Mac-composited virtual display above 60 Hz on Apple Silicon by any shipping
or reverse-engineered means. Why, precisely:
- **The cap is not in the API field.** `CGVirtualDisplayMode.refreshRate` is a `double` — you can
  build a 120 Hz mode and it's *accepted*. It's then **silently coerced/ignored**: WindowServer/
  CoreDisplay composites virtual displays at a fixed 60 Hz. The clamp lives in Apple's compositor
  layer (and, below it, the DCP display-coprocessor firmware), not in anything you call.
- **No DriverKit escape hatch.** Apple Silicon has **no public display/framebuffer DriverKit
  class** (DriverKit = USB/PCI/HID/net/audio/SCSI/serial only). Kexts are dead on M-series. Since
  M1 the display driver moved *out of the kernel* into DCP firmware — there is no signable
  userspace surface to inject a custom display mode. A "120 Hz virtual display driver" is **not
  buildable**, not just un-entitled.
- **Apple itself is capped.** Sidecar and AirPlay-to-Mac run their virtual displays at 60 Hz on
  Apple Silicon (M5 Vision Pro only got 120 Hz via *new silicon + new compositor work*). Apple
  not exceeding its own limit ⇒ deliberate, hard, OS-wide.
- **Universal third-party failure.** BetterDisplay, BetterDummy, Deskpad, Lumen, Sunshine-on-mac
  all inherit the 60 Hz cap; UFO-test measurements on 120/144/165 Hz hardware all read 60. (The
  "90/120 Hz Sunshine" capability is Windows-only — Apollo's `SudoVDA` driver — a red herring.)

### 6.2 The one cheap experiment to be 100% certain (~10 min, do it once)
Every source above tested via *mirroring* (which harmonizes rates down). RustScreen creates a
*fresh standalone* virtual mode and captures it directly via SCK — a slightly different path, so
falsify it on our exact code before believing the verdict:
1. Set `DisplayConfig::new(W, H, 120.0)` (`p5_stream.rs:282`; refresh propagates to the
   `CGVirtualDisplayMode` via `cg-virtual-display/src/config.rs:102`, `lib.rs:159`).
2. Read back: `CGDisplayCopyDisplayMode(id)` → `CGDisplayModeGetRefreshRate()`. If 60 → coerced.
3. Count SCK `stream_did_output` callbacks/sec for 5 s (`p5_stream.rs:87`). ~60 ⇒ capture is 60.
Expected: both report 60 → stop. If either shows 120, the verdict is overturned and immediately
exploitable via "capture at 120, pre-encode-drop to 60" (drop site: `return` before
`encode_frame_with_output_handler`, `p5_stream.rs:167`).

### 6.3 Capture-side phase latency (independent of refresh) — little to gain
- **Lowering/removing `minimumFrameInterval` does NOT reduce latency.** It's a *rate cap*, not a
  delivery clock — SCK already delivers each composited frame ASAP (change-driven). A smaller
  value can't pull a frame earlier than the 60 Hz compositor made it; it only lets sub-vsync
  bursts through → **decoder pile-up** (a regression). **Keep `1/60`.**
  - ⚠️ **Real bug found:** the *live* file currently sets **no `minimumFrameInterval`** (only the
    `pr24` branch adds `1/60`) — so it runs uncapped and exposed to burst pile-up. **Port the
    `1/60` cap into the live `p5_stream.rs` capture config.** (New TODO — fold into TODO #2 area.)
- **CGDisplayStream: dead.** Obsoleted in macOS 15; won't compile on this machine's SDK. Skip.
- **Direct IOSurface / CVDisplayLink bypass:** no supported path, sub-ms upside, breakage risk.
  Sampling faster than the compositor just yields duplicate frames (zero freshness). Not worth it.

### 6.4 The only real ways to actually beat the floor (and their cost)
1. **Capture a REAL ≥120 Hz display instead of a virtual one.** SCK genuinely delivers 120 fps
   from a physical ProMotion panel. On a base M1 Air (60 Hz panel) this needs a **headless 120 Hz
   external display** (real monitor or an EDID-spoofing dongle) arranged as the desktop extension,
   which the phone then *mirrors*. **Tradeoff: the product shifts from "phantom extend display" to
   "mirror a (headless) high-Hz extension."** Caveat: M1 + recent macOS impose their own external-
   display pixel-clock/refresh limits — **must be verified empirically** on the actual machine.
   Realistic result combined with the host fixes: **~35–45 ms** (still bounded by the phone's
   60 Hz present). Effort M.
2. **Perceived-latency layer (the highest felt-ROI, no new hardware, no floor change).** Even at a
   60 Hz video floor, you can make it *feel* multiples faster — this is why Parsec/RDP feel snappy:
   - **Local cursor:** hide the host cursor, render the pointer on the Pixel immediately from local
     input, decoupled from the video. Natural fit for RustScreen's touch back-channel (draw a
     synthetic pointer/touch-ripple the instant the finger lands, before the Mac round-trips it as
     video). **Felt cursor latency ~10–20 ms.** Effort S–M.
   - **Dirty-rect / changed-region fast-path:** ship small UI deltas as tiles instantly instead of
     a full-frame H.264 cycle (DisplayLink's trick), falling back to H.264 for large/animated
     regions. Watch the AOA bandwidth budget (don't send raw full-screen tiles). Effort M–L.

### 6.5 Round-3 net (what to actually do, hacker edition)
- **The refresh cap is a genuine hard wall** — stop trying to defeat `CGVirtualDisplay` (run 6.2
  once to confirm, then drop it). Don't burn time on DriverKit/kext/private-API rewrites; they're
  architecturally closed on Apple Silicon.
- **Biggest *felt* win:** the perceived-latency layer (6.4.2) — local cursor + dirty-rect. Cheap,
  no hardware, stacks with everything. **Start here if "feels fast" is the goal.**
- **Biggest *measured-floor* win without new hardware:** still the host queueing fixes
  (TODO #1–#4) — they reclaim 30+ ms that dwarfs the 8.3 ms capture floor. **Start here if the
  high-speed-camera number is the goal.**
- **Only true floor-mover:** capture a real 120 Hz source (6.4.1) — worth it only if you accept the
  mirror-a-headless-display product shift and verify M1 external-refresh limits.
- **Fix the live `minimumFrameInterval` regression** (6.3) regardless.

## Sources
Round 3: [BetterDisplay #121 — virtual display 60 Hz cap, UFO tests](https://github.com/waydabber/BetterDisplay/discussions/121) ·
[BetterDisplay #140 — "until Apple adds ProMotion to CGVirtualDisplay"](https://github.com/waydabber/BetterDisplay/issues/140) ·
[CGVirtualDisplayMode.h (refreshRate is a double)](https://github.com/w0lfschild/macOS_headers/blob/master/macOS/Frameworks/CoreGraphics/1336/CGVirtualDisplayMode.h) ·
[DriverKit families (no display class)](https://developer.apple.com/documentation/driverkit) ·
[IOMobileFramebuffer moved to DCP firmware](https://theapplewiki.com/wiki/IOMobileFramebuffer) ·
[Asahi DCP / presentation-timestamp gating for >60 Hz](https://asahilinux.org/2026/02/progress-report-6-19/) ·
[Sidecar 60 Hz on Apple Silicon](https://forums.macrumors.com/threads/ipad-pro-as-second-monitor-through-sidecar-why-limited-to-60hz.2353768/) ·
[M5 Vision Pro 120 Hz unlock (new silicon)](https://appleinsider.com/articles/25/10/17/new-apple-vision-pro-with-m5-doubles-refresh-rate-of-mac-virtual-display) ·
[Lumen (mac, inherits cap)](https://github.com/trollzem/Lumen) · [Apollo SudoVDA (Windows-only)](https://github.com/ClassicOldSong/Apollo) ·
[OBS PR #11896 — SCK 120 fps only on real ProMotion](https://github.com/obsproject/obs-studio/pull/11896) ·
[CGDisplayStream obsoleted macOS 15 (FreeRDP #10558)](https://github.com/FreeRDP/FreeRDP/issues/10558) ·
[Moonlight local-cursor requests #465](https://github.com/moonlight-stream/moonlight-qt/issues/465) ·
[DisplayLink regional compression (Tom's Hardware)](https://www.tomshardware.com/reviews/usb-graphics-adapter,3006-4.html).

Round 2: [BetterDisplay #121 — virtual displays locked 60 Hz on Apple Silicon](https://github.com/waydabber/BetterDisplay/discussions/121) ·
[AndroidPolice — Pixel 6a 90 Hz mod (brick risk)](https://www.androidpolice.com/pixel-6a-90hz/) ·
[XDA — Pixel 6a 90 Hz panel driver](https://xdaforums.com/t/the-90hz-panel-driver-for-the-pixel-6a-has-been-open-sourced.4528701/) ·
[AOSP — SurfaceView → hardware overlay; TextureView GL-composited](https://source.android.com/docs/core/graphics/arch-tv) ·
[AOSP — VSync / newest-buffer latch](https://source.android.com/docs/core/graphics/implement-vsync) ·
[Blur Busters — frame-age vs refresh](https://forums.blurbusters.com/viewtopic.php?t=14070) ·
[IOSurface zero-copy CVPixelBuffer](https://medium.com/lightricks-tech-blog/efficient-image-processing-in-ios-part-2-a96f0343e6f0) ·
[OBS PR #11896 — SCK 120 fps only on ProMotion source](https://github.com/obsproject/obs-studio/pull/11896).

Host: [WWDC21 low-latency VideoToolbox](https://developer.apple.com/videos/play/wwdc2021/10158/) ·
[EnableLowLatencyRateControl](https://developer.apple.com/documentation/videotoolbox/kvtvideoencoderspecification_enablelowlatencyratecontrol) ·
[MaxFrameDelayCount](https://developer.apple.com/documentation/videotoolbox/kvtcompressionpropertykey_maxframedelaycount) ·
[ForceKeyFrame](https://developer.apple.com/documentation/videotoolbox/kvtencodeframeoptionkey_forcekeyframe) ·
[DataRateLimits QA1958](https://developer.apple.com/library/archive/qa/qa1958/_index.html) ·
[WWDC22 ScreenCaptureKit](https://developer.apple.com/videos/play/wwdc2022/10155/) ·
[queueDepth](https://developer.apple.com/documentation/screencapturekit/scstreamconfiguration/queuedepth).
Phone: [Moonlight MediaCodecDecoderRenderer](https://github.com/moonlight-stream/moonlight-android/blob/master/app/src/main/java/com/limelight/binding/video/MediaCodecDecoderRenderer.java) ·
[ExoPlayer #8514 poc_type](https://github.com/google/ExoPlayer/issues/8514) ·
[Android low-latency media](https://source.android.com/docs/core/media/low-latency-media) ·
[Pixel 6a 60 Hz](https://www.androidauthority.com/google-pixel-6a-refresh-rate-3195345/).
USB: [nusb EndpointWrite](https://docs.rs/nusb/latest/nusb/io/struct.EndpointWrite.html) ·
[AOSP AOA protocol](https://source.android.com/docs/core/interaction/accessories/protocol) ·
[USB 2.0 HS bulk throughput](https://www.beyondlogic.org/usbnutshell/usb4.shtml).
Competitors: [Astropad Luna latency (11.3 ms USB)](https://astropad.com/blog/comparing-latency-luna-display/) ·
[scrcpy video.md (buffer=0)](https://github.com/Genymobile/scrcpy/blob/master/doc/video.md) ·
[Parsec technology](https://parsec.app/blog/description-of-parsec-technology-b2738dcc3842) ·
[Moonlight FAQ](https://github.com/moonlight-stream/moonlight-docs/wiki/Frequently-Asked-Questions) ·
[WebRTC playout-delay=0](https://webrtc.googlesource.com/src/+/main/docs/native-code/rtp-hdrext/playout-delay/README.md).
Rust: [thread_policy_set](https://developer.apple.com/documentation/kernel/1418892-thread_policy_set) ·
[rtrb](https://crates.io/crates/rtrb) · [bytes::BytesMut](https://docs.rs/bytes/latest/bytes/struct.BytesMut.html).
