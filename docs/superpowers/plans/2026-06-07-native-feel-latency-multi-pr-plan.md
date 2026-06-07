# Native-Feel Latency: Multi-PR Optimization Plan

> **Scope.** This is the execution plan for **Active-Priority 3 — "Native" feel** (see
> `.planning/ROADMAP.md`): drive glass-to-glass latency from the current best **~80 ms p50**
> down to a **consistent < 50 ms p50 with no visible jitter**, so dragging a window or moving
> the cursor on the Mac looks real-time on the phone. It splits the remaining latency work into
> **seven independently-shippable PRs**, ordered by leverage, with a measurement gate between
> each so every change is verified against the on-device per-stage report — not assumed.
>
> **Prime directive (CLAUDE.md):** the lowest-latency correct option wins; never add unbounded
> buffering on the hot path; bound every inter-stage queue to 1–2 and drop-oldest (drop-to-keyframe
> for coded video); measure, don't guess. Every PR below restates the tradeoff it makes.

> ## ✅ Updated 2026-06-07 — < 50 ms TARGET MET (glass-to-glass 33.7 ms p50 on device)
>
> The path there was not the one this plan predicted — measurement redirected it twice:
> 1. **PR 1 (non-blocking USB writes) REGRESSED** (~80 → ~100 ms p50): the per-frame `flush()` was
>    load-bearing flow control, not waste. **Reverted.** (research doc §7)
> 2. **The real bottleneck was a drain-loop bug on the phone:** `drain_output` blocked the full
>    10 ms `DEQUEUE_TIMEOUT_US` on its trailing "any more?" poll **every frame**, stalling the
>    single read+decode thread ~10 ms/frame. That compounded into a ~30–42 ms standing queue
>    (`send→arrive`) and was mis-measured as `decode→present` (~12.7 ms). **Fixing it
>    (block only for the first buffer, poll the rest with timeout 0) collapsed both stages and
>    took glass-to-glass to 33.7 ms p50 / 60 ms p95** — under target, at the ~43 ms hardware floor
>    once the ~1-vsync present-stamp offset is added back. (research doc §8)
>
> **Net result on device:** `send→arrive` 30–42 → 0.5 ms · `decode→present` 12.7 → 1.8 ms ·
> **GLASS→GLASS ~110 → 33.7 ms p50.** PR 4 (`releaseOutputBufferAtTime`) shipped too but was ~neutral
> — the present was never the cost.
>
> **What's left is polish, not median** (the plan's own stop rule: with p50 < 50 ms the rest is
> jitter/robustness): `arrive→decode` (~20 ms, Tensor decode floor) is now the largest stage; the
> p95 (~60 ms) + a one-time cold-start spike are the remaining tails → PR 6 (IDR bubble) / PR 2 (RT
> scheduling) / PR 7 (cold start). **PR 5 (phone RX thread) is no longer needed for the target** —
> the single thread keeps up once it isn't stalling 10 ms/frame; keep it in reserve only if a future
> p95 push needs max-not-sum.

---

## 1. Where we are (measured baseline)

The pipeline is **capture → VideoToolbox H.264 encode → AOA/USB → MediaCodec decode → present**.
The full root-cause history is in
[`docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md`](../research/2026-06-05-glass-to-glass-latency-root-cause.md).
What it established, and what is **already shipped**:

| Lever (already landed) | Where | Effect |
|---|---|---|
| Phone input-side pacing (`InputPacer`, drop-to-keyframe by in-flight depth) | `android-client/src/pacing.rs`, `session.rs` | `arrive→decode` **~500 ms → ~10 ms p50** |
| Exynos vendor low-latency key | `android-client/src/mediacodec.rs:253` | decoder emits eagerly (generic `KEY_LOW_LATENCY` is a no-op on Tensor) |
| Host encoder in-flight pacing (cap 2, freshest-wins, IDR resync on post-encode drop) | `macos-host/src/serve.rs:111-246` | bounds `capture→encode` bufferbloat |
| VideoToolbox low-latency rate control (`EnableLowLatencyRateControl`, RealTime, no B-frames, 20 Mbit cap, 1 s GOP) | `macos-host/src/serve.rs:413-502` | one-in/one-out encode, small frames fit the ~103 Mbit AOA link |
| Host drop-to-keyframe coalesce on the send channel | `macos-host/src/session.rs:412` (`coalesce_to_latest_keyframe`) | host queue residence ≤ one keyframe interval |
| Bounded push→pull bridge (`sync_channel(2)` + `try_send`) | `macos-host/src/serve.rs:525` | producer can't outrun the USB consumer |

**Current measured state (last on-device run):** glass-to-glass best **~80 ms p50**, drifting to
~158 ms under load. The per-stage decomposition that the remaining PRs attack:

| stage | measured | nature of the cost | attacked by |
|---|---|---|---|
| `capture→encode` | ~31 ms p50, spiking to 105 ms under motion | encode hold + residual queueing + jitter | PR 2 (jitter), PR 6 (IDR bubble) |
| `encode→send` | **~22 ms p50** | **per-frame blocking `flush()`; the wire needs ~1–2 ms** | **PR 1 (biggest single win)** |
| `send→arrive` (USB) | ~0.3–0.8 ms | fine — wire is not the bottleneck | — |
| `arrive→decode` | ~10 ms p50, ~14 ms p95 | solved; per-frame Tensor decode floor | PR 3 (hints) |
| `decode→present` | ~12 ms (one vsync) | vsync coupling on the decode thread | PR 4 (vsync decouple) |

### The targets, and the floor (do not chase past this)

- **P5 goal: < 50 ms p50 glass-to-glass.**
- **Practical floor ≈ 43 ms:** the Pixel's 60 Hz panel (~8–16 ms present) + the locked-60 Hz
  `CGVirtualDisplay` capture cadence (~16 ms) are fixed by hardware. **Sub-~30 ms is not reachable**
  without new hardware. **Ruled out, do NOT attempt:** HEVC (anti-latency on Tensor — higher encode
  cost, no decode win), 90 Hz panel unlock (firmware-locked, brick risk), >60 fps source
  (`CGVirtualDisplay` hard-locked to 60 Hz on Apple Silicon).

### The unifying bug class (the lens for every PR)

> On a lossless USB link, **an unbounded queue at any stage is pure added latency.** Every PR below
> is an instance of the same fix: bound a stage to 1–2 deep, drop-oldest (drop-to-keyframe for coded
> video), and never *block* the hot path waiting for a slow downstream stage. We have applied this to
> the encode channel and the decoder input queue; PR 1 applies it to the USB writer, PR 5 to the
> phone's decode/render handoff.

---

## 2. The split — seven PRs, ordered by leverage

Each PR is independently shippable and independently measurable. **Do them in order**, and
**run the measurement gate after each** (§4): the bottleneck moves between stages as we fix
them, so the *next* PR's value is only knowable after the *previous* one is measured. Stop early
if a gate shows < 50 ms p50 consistently — the later PRs are jitter/robustness polish, not median wins.

| PR | Title | Primary stage | Expected win | Risk | Depends on |
|---|---|---|---|---|---|
| **1** | Non-blocking pipelined USB writes | `encode→send` | **~18 ms p50** | med | — |
| **2** | Real-time thread scheduling | p95/p99 jitter (all host stages) | spikes, not median | low | — |
| **3** | Phone decoder hints (`priority`, `operating-rate`) | `arrive→decode` | ~0–5 ms | low | — |
| **4** | Present-time pacing + vsync decouple | `decode→present` | ~5–12 ms p95 | med | — |
| **5** | Phone RX/decode/render thread split | phone total (max-not-sum) | jitter + tail | high | 3, 4 |
| **6** | On-demand keyframe back-channel + IDR-bubble smoothing | `capture→encode` periodic spike | bubble + resync cost | med | — |
| **7** | Cold-start & reconnect latency trims | time-to-first-frame | UX, not steady-state | low | — |

PRs 1–4 are independent and can be reviewed in parallel; 5 should follow 3+4 (it subsumes their
seams); 6 and 7 are independent of all of the above.

---

## 3. PR-by-PR detail

Each section gives: **goal · the lever · files & line refs · implementation sketch · tests ·
measurement gate · tradeoff · done-when.** Code sketches are guidance, not literal patches —
follow strict TDD (CONTRIBUTING.md): write the failing host test first where the logic is
platform-agnostic.

---

### PR 1 — Non-blocking, pipelined USB writes ⭐ (the single biggest remaining win)

> **Status: MEASURED — REGRESSED → REVERTED (GH PR #27).** The `StreamSink`/`submit()` change
> collapsed `encode→send` (22 → 0.2 ms p50) but regressed **glass-to-glass ~80 → ~100 ms p50**
> under load: the per-frame `flush()` was load-bearing flow control (a bulk-OUT transfer completes
> only when the phone pulls the bytes), and removing it relocated the 22 ms into `send→arrive` and
> added a ~3-frame standing queue in invisible USB/kernel buffers (`dropped = 0`). **Reverted to the
> known-good baseline.** Revisit only after PR 4/5 make the phone drain USB fast enough, and then
> with app-level in-flight accounting (not blind `submit`). Full data + diagnosis: research doc
> [§7](../research/2026-06-05-glass-to-glass-latency-root-cause.md).

**Goal.** Cut `encode→send` from ~22 ms to ~2–4 ms by removing the per-frame **blocking** flush
from the hot path, so the writer submits the next frame's bulk transfers without waiting for the
previous frame's to *complete* on the wire.

**The lever.** Today `send_encoded_frame` ([`macos-host/src/session.rs:603`](../../../crates/macos-host/src/session.rs))
calls `out.flush()` after every frame, and on the AOA path `flush()`
([`aoa.rs:366`](../../../crates/macos-host/src/aoa.rs)) blocks until nusb's in-flight bulk-OUT
transfers *drain*. We already pipeline depth-4 transfers (`set_num_transfers(4)`,
[`serve.rs:601`](../../../crates/macos-host/src/serve.rs)), but the blocking flush throws that
pipelining away: each frame waits for its own bytes to land before the next is submitted. The wire
itself moves a ~40 KB frame in ~1–2 ms; the other ~20 ms is the writer parked in `flush()`.

**Implementation sketch.**
- Introduce a `submit()`-vs-`flush()` distinction on `AoaWriteHalf`: `submit` hands the frame's
  bytes to nusb (queues the bulk-OUT transfers) and returns **without** waiting for completion;
  a real `flush`/`drain` is called only where ordering against a *read* demands it.
- Change the per-frame send path to **submit, not block-flush**. The depth-4 transfer ring already
  bounds how far ahead the writer can run; that ring *is* the bound (do not also grow a software
  queue). When the ring is full, `submit` naturally applies backpressure — that is the desired
  pace-to-the-wire behavior, not a stall to fix.
- **Preserve invariant C-01** ("no unflushed partial frame before a blocking read"). The
  `FlushAuditTransport` test ([`session.rs:793`](../../../crates/macos-host/src/session.rs)) encodes
  this: a blocking read must see zero unflushed bytes. The streaming loop only reads on the *split*
  read half (a separate thread), so the write half never interleaves a read between frames — but the
  **handshake and clock-sync** (`perform_handshake`, `perform_clock_sync`) do write-then-read on the
  duplex transport and **must keep a true blocking flush**. Keep `flush()` honest there; only the
  post-split streaming send switches to `submit`.
- Keep the existing finite `set_write_timeout(5s)` ([`serve.rs:610`](../../../crates/macos-host/src/serve.rs))
  so a wedged endpoint still becomes a clean session-end rather than an infinite hang.

**Tests (host, no hardware).**
- New `submit` semantics on a fake write half: N frames submitted without an intervening blocking
  drain; bytes all present after an explicit final drain.
- C-01 regression: extend `FlushAuditTransport` usage to prove the handshake/clock-sync path still
  flushes before its read, while the streaming path is allowed to leave bytes in-flight between
  frames (a *new* audit mode that counts "submitted but not yet drained" separately from "lost").

**Measurement gate.** `encode→send` p50 drops from ~22 ms toward ~2–4 ms; glass-to-glass p50 drops
by ~15–18 ms. Watch `send→arrive` stays ~sub-ms (if it rises, the phone reader is now the limiter →
that's PR 5 territory). Confirm no rise in dropped-frame count (a too-shallow transfer ring would
force drops).

**Tradeoff.** Slightly more bytes can be "in the air" if the phone stalls mid-frame — but the
depth-4 ring + 5 s write timeout already bound that, and a post-encode drop already forces an IDR
resync, so correctness is preserved. Pure latency win, no quality cost.

**Done when.** `encode→send` p50 < 5 ms on device and glass-to-glass p50 improves by ≥ 15 ms, C-01
tests green.

---

### PR 2 — Real-time thread scheduling for the hot-path threads

**Goal.** Remove the 30–60 ms **p95/p99 jitter spikes** (the "occasionally feels laggy" cases) by
promoting the capture-delegate, encode-handler, and USB-writer threads to real-time scheduling so
the macOS scheduler doesn't preempt them behind background work. This targets the **tail, not the
median.**

**The lever.** The capture delegate runs on a GCD queue (`com.rustscreen.stream`,
[`serve.rs:529`](../../../crates/macos-host/src/serve.rs)); the writer runs on the main thread; the
stats reader on a spawned thread. None request real-time QoS, so a busy system can delay a frame by
tens of ms at p95.

**Implementation sketch.**
- macOS: set the GCD queue's QoS to `QOS_CLASS_USER_INTERACTIVE`, and apply a **time-constraint
  thread policy** (`thread_policy_set` with `THREAD_TIME_CONSTRAINT_POLICY`) to the encode/send
  thread, sized to the 16.6 ms frame budget (period ≈ 16.6 ms, computation ≈ a few ms). Wrap this
  behind a small `realtime` helper module with a safe Rust API over the Mach FFI, isolated like the
  other platform shims.
- Phone (paired, optional in this PR): request `KEY_PRIORITY = 0` (realtime) on the decoder — folded
  into PR 3 if cleaner.
- Gate everything so a failure to acquire RT scheduling **degrades to normal priority with a log
  line**, never a hard error (RT policy can be denied under sandbox/hardened-runtime).

**Tests.** The Mach FFI can't be unit-tested for effect, but the helper's argument computation
(period/computation/constraint from a target frame interval) is pure and TDD-able. The on-device
proof is the p95/p99 columns of the report.

**Measurement gate.** `capture→encode` and `encode→send` **p95/max** drop sharply (the median barely
moves). Glass-to-glass max/p99 tightens toward p50. Run a *high-motion* scene (drag a window
continuously) — that is where the spikes appear.

**Tradeoff.** RT threads can starve other work if mis-sized; the time-constraint policy bounds CPU
so it can't monopolize. Worth it — jitter is the difference between "smooth" and "native".

**Done when.** Glass-to-glass p99 within ~1.5× of p50 under continuous motion; no regression in p50.

---

### PR 3 — Phone decoder hints (`priority`, `operating-rate`)

**Goal.** Squeeze the last few ms off `arrive→decode` (and protect it under load) with two
one-line decoder format hints. Cheap, low-risk, A/B-testable on device.

**The lever.** At configure time ([`mediacodec.rs:234-253`](../../../crates/android-client/src/mediacodec.rs))
we set MIME/width/height/`KEY_LOW_LATENCY`/vendor-key/csd-0. We do **not** set:
- `KEY_PRIORITY = 0` — request realtime decode priority.
- `KEY_OPERATING_RATE` — set to a high value (e.g. `i32::MAX` or 240) to tell the codec to run its
  clock as fast as possible rather than pacing to the nominal frame rate.

**Implementation sketch.** Add two `AMediaFormat_setInt32` calls next to the existing low-latency
keys, with the same "unknown key is silently ignored, safe to set unconditionally" rationale already
documented for the vendor key. Confirm the bound component is the *hardware* `c2.exynos.h264.decoder`
(not the software `c2.android.avc.decoder`) via a logcat check — the research doc flags the
silent-software-fallback risk.

**Tests.** Device-only (the module is `#[cfg(target_os = "android")]`); verification is a
cross-compile type-check + clippy + the on-device `arrive→decode` reading.

**Measurement gate.** `arrive→decode` p50/p95 holds or drops; most importantly it should **not grow**
under sustained high motion. A/B with the keys removed to confirm the delta is real (the research doc
warns these hints are SoC-dependent — measure, don't assume).

**Tradeoff.** `operating-rate` high can raise power draw slightly; acceptable on a wired link where
the phone trickle-charges. If a gate shows no benefit, keep only `KEY_PRIORITY` and document the
null result.

**Done when.** Confirmed hardware decoder bound; `arrive→decode` stable under load; A/B result
recorded.

---

### PR 4 — Present-time pacing + vsync decouple (research category D)

> **Status: implemented (GH PR #27); the REAL win turned out to be a drain-loop bug, not the present.**
> Two changes landed here:
> 1. Present via `AMediaCodec_releaseOutputBufferAtTime(idx, monotonic_now_ns())` instead of the
>    blocking boolean form (correct non-blocking present; turned out to be ~neutral on device).
> 2. **The actual fix:** `drain_output` was polling `dequeueOutputBuffer` with the full 10 ms
>    `DEQUEUE_TIMEOUT_US` on *every* iteration, so the trailing "any more?" poll **blocked ~10 ms
>    per frame** on the single decode thread. On-device this was being mis-read as `decode→present`
>    (~12.7 ms, stuck regardless of the present call — the tell that the present wasn't the cost),
>    and it also delayed the next USB read → inflated `send→arrive`. Now we block only for the
>    *first* buffer and poll the rest with timeout 0.
>
> Type-checks + clippy clean on `aarch64-linux-android`. **Measurement gate:** `decode→present`
> should collapse toward sub-ms and `send→arrive` should drop (thread reads USB ~10 ms sooner/frame).
> If `send→arrive` is still large after this, the single thread genuinely can't keep up → PR 5
> (dedicated RX thread) is next. **Lesson:** a stage stuck at exactly one timeout value is a
> blocking-poll artifact, not real latency — check the timeout before believing the stage.

**Goal.** Remove the ~12 ms vsync stall currently charged to `decode→present` by switching from the
**blocking** boolean render to the **timestamped** render, letting SurfaceFlinger schedule/drop the
present instead of blocking the decode thread on the next vsync.

**The lever.** `drain_output` renders the newest buffer with
`AMediaCodec_releaseOutputBuffer(codec, idx, true)`
([`mediacodec.rs:492`](../../../crates/android-client/src/mediacodec.rs)) — the boolean form, which
blocks the decode thread until vsync once the BufferQueue isn't saturated (the research doc §"red
herring" documents the 1 ms → 13 ms rise this caused). Android offers
`AMediaCodec_releaseOutputBufferAtTime(codec, idx, nanoTime)`: hand the present a target timestamp
and return immediately; SurfaceFlinger composites at that time and drops late frames itself.

**Implementation sketch.**
- Replace the render call with the at-time form, target = `now` (present ASAP) or `now + small
  phase` derived from the panel's vsync if we want jitter-free cadence. Start with `now` (lowest
  latency) and only add a phase offset if the panel shows tearing/judder.
- Keep stamping `present_us` right after the call; with the non-blocking form this now measures the
  *handoff*, not the vsync wait — note that in the latency doc so the number is interpreted correctly
  (the real present is ~one vsync later, but it no longer *blocks the decoder*, which is the win).
- Continue dropping stale buffers with `render=false` ([`mediacodec.rs:478`](../../../crates/android-client/src/mediacodec.rs)).

**Tests.** Device-only render path; the pure parts (target-time computation if a phase offset is
added) are TDD-able. Proof is the `decode→present` reading + eyeball smoothness.

**Measurement gate.** `decode→present` stops being pinned at ~one refresh on the decode thread;
the decode thread frees up (visible as steadier `arrive→decode` under load). Glass-to-glass p95
tightens. Eyeball: no new judder/tearing.

**Tradeoff.** At-time release can drop a frame SurfaceFlinger deems late — exactly what we want on a
latency-first link (a dropped late present beats a blocked decoder). If judder appears, add a small
fixed vsync-phase offset (costs a few ms of latency for cadence) and document the choice.

**Done when.** Decode thread no longer blocks on vsync; glass-to-glass p95 improves; no visible
judder.

---

### PR 5 — Phone RX / decode / render thread split (research category C)

**Goal.** Make the phone pay **max(stage)** not **sum(stage)** per frame by splitting the
single-threaded receive loop into RX → decode → render stages connected by **bounded, drop-oldest**
queues (cap 1–2), mirroring Moonlight's assembly-line. This is the biggest *structural* phone change
and the one that most improves behavior under burst/jitter.

**The lever.** `run_usb_fd` ([`android-client/src/lib.rs:150`](../../../crates/android-client/src/lib.rs))
runs USB-read + decode + present inline on one thread (`run_session` →
`session.rs` receive loop). Per-frame cost is the sum of all three; a hiccup in any one stalls the
others. The seams already exist: `session.rs` is platform-agnostic, the transport is injectable, and
`InputPacer` already centralizes the drop-to-keyframe decision.

**Implementation sketch.**
- **RX thread:** read frames off USB → bounded ring (cap 2, drop-oldest with the existing
  drop-to-keyframe rule from `pacing.rs`, reused so the logic stays in one place).
- **Decode thread:** pull from the RX ring → `decoder.decode()` → bounded ring (cap 1–2).
- **Render thread:** present newest (using PR 4's at-time release), drop the rest.
- Keep all rings **leaky/drop-oldest** — never grow. Reuse `InputPacer`/`coalesce` semantics for the
  coded-video drop rule. Preserve the `Frame::Stats` back-report (now emitted from the render stage).
- Preserve the connect-hello / surface-rendezvous ordering (`rendezvous.rs`); the split happens
  *after* the surface is acquired.

**Tests (host).** The ring + drop-oldest logic is pure and CI-testable against fakes (extend the
existing `RecordingDecoder` pattern). Thread orchestration gets a host integration test with an
in-memory transport feeding a scripted burst, asserting only the freshest decodable frames present
and depth stays bounded.

**Measurement gate.** Under a bursty source (rapid window drags), glass-to-glass p95/p99 tightens
markedly vs. the serial loop; `input_frames_dropped` rises only during genuine bursts and returns to
~0 in steady state. Median should be ≤ the serial-loop median (max-not-sum can only help).

**Tradeoff.** More moving parts and thread-safety surface (this is the "high risk" PR). It is the
research doc's "replaceable-later architectural ideal" — only worth it if PRs 1–4 haven't already
landed us comfortably < 50 ms p50. **Gate decision: skip or defer PR 5 if PR 1–4 measure
consistently < 50 ms p50 with tight p99.**

**Done when.** Phone per-frame cost ≈ max not sum (provable by comparing single-stage timings to
total under load); bounded depth proven; no deadlock across surface destroy/recreate.

---

### PR 6 — On-demand keyframe back-channel + IDR-bubble smoothing (categories E + F)

**Goal.** Remove the **periodic latency bubble** from the 1 s full-IDR GOP and **bound the cost of
any drop-to-keyframe resync** by wiring a keyframe-request back-channel and (optionally) spreading
I-macroblocks across frames (intra-refresh) for uniform frame sizes.

**The lever — two parts.**
1. **Keyframe-request back-channel (F).** The protocol already has `Control::RequestKeyframe`, but
   the phone never sends it and the host never acts on it (the host reader at
   [`serve.rs:632`](../../../crates/macos-host/src/serve.rs) only handles `ClockPong`/`Stats`). Wire
   it: when the phone's `InputPacer` enters drop mode (it just dropped to await a keyframe), have it
   send `RequestKeyframe`; the host, on receipt, sets `needs_keyframe` (the same flag the post-encode
   drop already uses, [`serve.rs:206`](../../../crates/macos-host/src/serve.rs)) to force an IDR on
   the next frame. This bounds a resync to ~one frame instead of up to ~1 s (the periodic GOP).
2. **IDR-bubble smoothing (E).** A full IDR every 60 frames is a large access unit that spikes both
   encode time and USB transfer — a periodic `capture→encode`/`encode→send` bump. Options, in
   preference order: (a) with F landed, **lengthen the periodic GOP** (rely on on-demand keyframes
   for resync) so full IDRs are rare; (b) if VideoToolbox exposes it, enable **intra-refresh** so
   I-MBs spread across P-frames for uniform frame sizes.

**Implementation sketch.**
- Phone: emit `Control::RequestKeyframe` from the drop-mode transition in the receive loop
  (`session.rs`), rate-limited (at most one outstanding request) so a burst doesn't spam the host.
- Host reader thread: add a `RequestKeyframe` arm that sets `needs_keyframe` (reuse the existing
  atomic + force-IDR path — no new encode mechanism).
- Host encoder: make `MaxKeyFrameInterval` configurable; with F working, raise it (fewer forced full
  IDRs). Probe `kVTCompressionPropertyKey_...` for an intra-refresh / max-frame-delay-style key; if
  absent, stop at (a) and document that intra-refresh isn't available.

**Tests (host).** The phone's request-on-drop logic is pure (extend `InputPacer`/session tests). The
host's `RequestKeyframe → needs_keyframe` handling is unit-testable against the reader's frame
dispatch. The encoder property change is device-verified.

**Measurement gate.** The periodic ~1 s spike in `capture→encode`/`encode→send` max flattens. After a
forced drop-to-keyframe, time-to-clean-image is ~one frame (visible as a short `input_frames_dropped`
burst that clears immediately, not a ~1 s garbage/freeze window). Steady-state p50 unchanged or
slightly better.

**Tradeoff.** Lengthening the GOP without F would make loss recovery slow — so F **must** land with
(a). Correctness (clean resync) is preserved because the request guarantees a bounded keyframe.

**Done when.** `RequestKeyframe` works end-to-end (provable on device by forcing a drop and seeing a
sub-100 ms recovery); periodic encode/send spike removed.

---

### PR 7 — Cold-start & reconnect latency trims

**Goal.** Shrink **time-to-first-frame** (cold start and every reconnect) so the screen "comes back
instantly". This is UX latency, not steady-state glass-to-glass, but it is part of "native feel".

**The lever (from the ROADMAP Priority 1/2 sub-tasks).**
- Replace the fixed `sleep(500ms)` after virtual-display creation
  ([`serve.rs:364`](../../../crates/macos-host/src/serve.rs)) with a **poll** of
  `SCShareableContent` until the display appears (~350 ms saved, no phantom-display hang).
- Lower the ~10 s surface rendezvous ceiling ([`lib.rs:163`](../../../crates/android-client/src/lib.rs))
  to ~3 s (it only ever needs to cover the surface-vs-fd ordering race).
- **Force a keyframe on (re)connect** (rides on PR 6's `needs_keyframe` mechanism) so the first frame
  after a connect is a clean IDR within ~16 ms instead of garbage until the next periodic GOP.
- (If `nusb` hotplug lands per Priority 2) subscribe before `req53` to shave ~50 ms off detect.

**Tests.** The poll-with-timeout helper is pure and TDD-able (deadline logic). The rest is on-device
timing of "plug in → first frame".

**Measurement gate.** Wall-clock from app-open to first rendered frame drops by ~0.4–0.5 s; first
frame is clean (no garbage). Reconnect (close/reopen phone app) re-establishes a clean image within
a couple of seconds.

**Tradeoff.** None of substance — replacing a fixed sleep with a bounded poll is strictly better.
Keep a generous poll ceiling so a slow display-create still succeeds rather than erroring.

**Done when.** Cold start visibly faster, first frame clean on every connect.

---

## 4. Measurement protocol (run after every PR)

Every PR is verified against **real device numbers**, never assumption (CLAUDE.md). The pipeline is
already instrumented end-to-end (SNTP clock-sync + per-stage + glass-to-glass via
`PipelineLatency`, printed by `print_report` in [`serve.rs:765`](../../../crates/macos-host/src/serve.rs)).

**Procedure (the user runs the capture — a Claude-launched process can't hold the Screen Recording grant):**

1. Build release (debug materially worsens encode/copy latency):
   ```bash
   cargo install --path crates/macos-host --bin rustscreen --features live-capture,live-usb
   ```
   and the phone APK with `--features live-decode` (see `README.md` / the APK-build notes).
2. Run the host from a terminal holding **Screen & System Audio Recording**, phone plugged in & app open:
   ```bash
   cargo run --release -p macos-host --features "live-capture live-usb" --bin p5_stream
   ```
3. Capture **two scenes**: (a) light motion (cursor move) for the median; (b) **continuous window
   drag** for the tail — jitter only shows under sustained motion.
4. Read the per-stage report: `capture→encode`, `encode→send`, `send→arrive`, `arrive→decode`,
   `decode→present`, `GLASS→GLASS`, each as **avg / p50 / p95 / max**, plus the `dropped frames` and
   phone `input-dropped` counters (`adb logcat -s RustScreen:*`).
5. **Record before/after in the latency-budget table** and append the result to the research doc, the
   same way item A+B was recorded (`...root-cause.md` §6). A PR is not done until its gate row is filled.

**Pass criteria per PR:** the stage it targets improves as predicted **and** no other stage
regresses beyond noise (±rtt/2 precision is printed on every fused number). The overall exit
criterion for Priority 3: **glass-to-glass consistently < 50 ms p50 with p99 within ~1.5× of p50**,
and dragging a window on the Mac looks real-time on the phone.

---

## 5. Cross-cutting rules (apply to every PR)

- **Bound, never grow.** Every queue/ring introduced or touched is cap 1–2, drop-oldest; coded-video
  drops are drop-to-keyframe and force a resync (the `needs_keyframe` / `InputPacer` mechanisms).
- **Don't block the hot path.** No blocking call, heap allocation, lock contention, or vtable
  dispatch added to capture/encode/send/decode/present where a cheaper option exists.
  `Ordering::Relaxed` for standalone hot-path counters/flags.
- **Correctness still wins over latency when they genuinely conflict** — but say so. Each PR above
  states its tradeoff explicitly; the default is the lower-latency path.
- **TDD + commit hygiene (CONTRIBUTING.md / CLAUDE.md):** failing host test first for any
  platform-agnostic logic; `clippy -D warnings`; `cargo fmt --check`; remove unused imports from every
  changed file before committing; MSRV 1.80.
- **Stop at the floor.** If a gate shows consistent < 50 ms p50 with tight p99, the remaining PRs are
  optional jitter/robustness polish — do not chase sub-43 ms numbers the hardware can't deliver.

## 6. Dependency graph & suggested merge order

```
PR1 ─┐
PR2 ─┤ (independent; review in parallel)
PR3 ─┼──► PR5 (thread split subsumes 3+4 seams)
PR4 ─┘
PR6  (independent — protocol back-channel + encoder)
PR7  (independent — startup; rides PR6's force-IDR for the clean-first-frame part)
```

**Recommended sequence:** PR 1 → measure → PR 2 → measure → PR 3 → PR 4 → measure → **decide on PR 5**
(skip if already < 50 ms p50 tight) → PR 6 → PR 7. PR 1 alone is expected to be the headline result.
