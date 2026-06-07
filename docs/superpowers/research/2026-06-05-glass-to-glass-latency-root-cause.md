# Glass-to-Glass Latency: Root Cause & Improvement Map

> Research artifact for roadmap item 6 / P5 criterion #2 (glass-to-glass < 50 ms).
> Written 2026-06-05 after two phone-side tuning rounds failed to move the dominant
> `arrive→decode` stage. Combines a line-level read of our code with external research
> on how Moonlight, scrcpy, Parsec, and WebRTC solve the same problem.

## TL;DR

The phone-side fixes (`KEY_LOW_LATENCY` + render-newest-drop-rest) **could not have
worked**, for a structural reason: we measure `decode_us` *after* the frame has already
waited in the codec **input** queue, but our drop logic only sheds frames on the
**output** side — downstream of the measurement point. We were dropping frames *after*
they had already paid the latency we were trying to remove.

The ~500 ms lives in the **MediaCodec input queue**: we read one frame off USB and
immediately `queueInputBuffer` it, every iteration, with **no bound on how many
undecoded frames pile up inside the codec**. The Pixel 6a is a Tensor/Exynos part whose
C2 decoder **ignores the generic `KEY_LOW_LATENCY`** (it needs the Exynos *vendor* key),
so it buffers ahead and the queue never self-drains. Three independent sources (Moonlight,
it-jim, getstream) name this exact failure: an unbounded producer/consumer mismatch in a
single-threaded serial loop.

---

## 1. Why nothing improved — the measurement/drop mismatch

### The data, restated
| stage | run 1 (old APK) | run 2 (new APK, LOW_LATENCY + drop-rest) |
|---|---|---|
| send→arrive (USB) | ~0.8 ms | ~0.8 ms |
| **arrive→decode** | **~290–550 ms, growing** | **~340–570 ms, still growing** |
| decode→present | ~1 ms | ~13 ms |
| GLASS→GLASS | ~470–620 ms | ~490–830 ms |

`send→arrive ≈ 0` means the phone reads frames off the USB endpoint promptly — **the
backlog is not in the wire or kernel buffer.** It is *after* arrival and *before* the
output dequeue. That window is exactly the codec input queue.

### The smoking gun in our code
- `arrive_us` is stamped in [`session.rs:315`](../../../crates/android-client/src/session.rs) right before `feed`.
- `decode_us` is stamped in [`mediacodec.rs:406`](../../../crates/android-client/src/mediacodec.rs) **at `dequeueOutputBuffer`** — i.e. the instant the decoder *emits* the frame.
- So `arrive→decode` = time the access unit spends sitting between `queueInputBuffer` and the matching `dequeueOutputBuffer` = **codec input-queue residence**.

Our "freshest frame wins" drop logic lives in `drain_output`
([`mediacodec.rs:431–461`](../../../crates/android-client/src/mediacodec.rs)): it collects
every *already-decoded* output buffer, renders the newest, and releases the rest with
`render=false`. **Every one of those buffers has already been dequeued — it already
incurred the full input-queue wait and already had `decode_us` stamped.** Dropping it now
removes a *render*, not a *latency*. Mathematically it cannot reduce `arrive→decode`.

> Analogy: we measured how long passengers wait in the security line, then "fixed" it by
> sending some of them home *after* they cleared security. The line is exactly as long.

### Why the queue exists and grows
The serial loop ([`session.rs:299`](../../../crates/android-client/src/session.rs)) reads
**one** frame per iteration and `decode()`
([`mediacodec.rs:290`](../../../crates/android-client/src/mediacodec.rs)) `queueInputBuffer`s
**every** frame it's handed. The decoder advertises many input buffers, so
`dequeueInputBuffer` keeps succeeding and we keep stuffing frames in. With the Tensor
decoder not emitting eagerly (see §2), output trails input, and because nothing ever
*drops on the input side*, the gap set at startup persists and slowly grows. The host's
own `capture→encode` growth in run 2 is the back-pressure wave from this: when the codec
finally stalls input, USB fills, and the host's bulk write blocks → host channel backs up.

### Why `decode→present` rose 1 ms → 13 ms (a red herring that looked like progress)
With render-rest dropped, the Surface BufferQueue is no longer saturated, so
`releaseOutputBuffer(newest, true)` ([`mediacodec.rs:453`](../../../crates/android-client/src/mediacodec.rs))
— the **boolean** form — now blocks the decode thread until the next vsync (~13 ms ≈ one
refresh). That's vsync coupling (§ category D), not a real improvement.

---

## 2. The Pixel 6a is Tensor/Exynos — the generic low-latency key is a no-op

`KEY_LOW_LATENCY` is honored mostly by Qualcomm/Adreno. The Pixel 6a's hardware AVC
decoder is `c2.exynos.h264.decoder`. Moonlight's `MediaCodecHelper` sets **vendor-specific**
keys per SoC; the Exynos one is:

```
vendor.rtc-ext-dec-low-latency.enable = 1
```

We only set the generic key at [`mediacodec.rs:238`](../../../crates/android-client/src/mediacodec.rs).
Also worth confirming we bind the *hardware* decoder and didn't silently fall back to the
software `c2.android.avc.decoder`. Moonlight further notes `FEATURE_LowLatency` is
sometimes present only on a *non-first* decoder, so they enumerate all and pick one that
advertises it. (Source: Moonlight `MediaCodecHelper.java`, AOSP low-latency-media docs.)

Even fully tuned, expect a higher per-frame floor on Tensor than Qualcomm
(Apollo #1308 reports 15–20 ms/frame on Tensor vs ~1 ms on Snapdragon) — which is *fine*
for a 50 ms budget once the queue backlog is gone. The backlog is where the 500 ms is.

---

## 3. Improvement map — categorized, with code locations

Ordered by leverage. A–B–G together should crack the 50 ms target; C–D are the
"replaceable-later" architectural ideal; E–F–H are supporting.

### A. Bound the input queue + drop-to-keyframe **on the input side** ⭐ highest leverage
**Problem:** we feed every frame; the codec input queue is unbounded.
**Where:** [`mediacodec.rs:290 decode()`](../../../crates/android-client/src/mediacodec.rs)
and the receive loop [`session.rs:299`](../../../crates/android-client/src/session.rs).
**Fix:** track in-flight depth = (frames queued − frames emitted). Before queuing a new
frame, if depth exceeds a small N (1–2), **don't feed it** — drop forward to the next
keyframe (mirror the host's `coalesce_to_latest_keyframe`, but pre-decode). Honor
`dequeueInputBuffer(0) == -1` as "codec is full, drop" rather than retry-and-stuff
(current code retries up to 100× — [`mediacodec.rs:308–325`](../../../crates/android-client/src/mediacodec.rs)).
This is the *only* change that attacks the stage where the latency actually is.
**Note:** drop-to-keyframe needs in-band keyframes available; we already send periodic IDR
every 60 frames, but consider an on-demand keyframe request (category F) so a forced
resync is bounded to << 1 s.

### B. Set the Exynos vendor low-latency key ⭐
**Where:** [`mediacodec.rs:238`](../../../crates/android-client/src/mediacodec.rs) (next to the generic key).
**Fix:** also `AMediaFormat_setInt32(format, "vendor.rtc-ext-dec-low-latency.enable", 1)`.
Probe `FEATURE_LowLatency` across decoders; assert we bound `c2.exynos.h264.decoder`.
Makes the decoder emit eagerly so depth stays shallow in the first place.

### C. Decouple receive / decode / render into threads with bounded drop-oldest queues (architectural ideal)
**Problem:** [`lib.rs:140 run_usb_fd`](../../../crates/android-client/src/lib.rs) runs the
whole pipeline inline on one thread, so per-frame cost is the **sum** of USB-read + decode
+ vsync-render instead of the **max**. This is the it-jim "serial loop vs assembly line"
failure.
**Fix (Moonlight's model):** RX thread → bounded ring (cap 2, drop-oldest) → decode thread
→ bounded ring (cap 1–2) → render thread. Each queue leaky/drop-oldest. Bigger change;
the seam already exists (`session.rs` is platform-agnostic, transport is injectable).

### D. Decouple vsync from the decode thread
**Where:** [`mediacodec.rs:453`](../../../crates/android-client/src/mediacodec.rs).
**Fix:** use the **timestamp** form `releaseOutputBuffer(index, nanoTime)` (lets
SurfaceFlinger drop late frames instead of blocking), and/or render on a separate thread
(see C). Removes the 13 ms vsync stall from the receive path.

### E. Encoder: avoid the IDR latency bubble (host side)
**Where:** [`p5_stream.rs:385–404`](../../../crates/macos-host/src/bin/p5_stream.rs).
Already good: `RealTime`, `AllowFrameReordering=false` (no B-frames), 20 Mbit cap. The
remaining lever is that a full **IDR every 60 frames** is a large frame that spikes USB
transfer and creates a periodic latency bubble. Options: **intra-refresh** (spread I-MBs
across P-frames for uniform frame sizes) if VideoToolbox supports it, or **on-demand
keyframes** (F) instead of a fixed 1 s GOP.

### F. Add a keyframe-request back-channel (WebRTC PLI analog)
**Where:** protocol already has `Control::RequestKeyframe`; the phone ignores Control
([`session.rs:359`](../../../crates/android-client/src/session.rs)) and the host doesn't
act on it. **Fix:** phone detects backlog/undecodable → sends `RequestKeyframe` → host
forces an IDR → phone drops-forward to it. Bounds the cost of an input-side drop (A) and
of any loss recovery.

### G. Drain the startup burst before steady state
**Problem:** during surface rendezvous (up to 10 s, [`lib.rs:152`](../../../crates/android-client/src/lib.rs))
and clock-sync, the host may stream frames that buffer and then get fed in one burst,
priming the codec input queue deep before the first output appears — exactly the depth
that then persists.
**Fix:** on entering the receive loop, non-blockingly drain the transport to the latest
keyframe before feeding (one-time application of A), or gate host streaming on a
phone-ready signal.

### H. Instrument the queue depth (close the diagnostic gap that hid this)
**Problem:** we measured `arrive→decode` but not **in-flight codec depth**, so a queue
problem read as "the decoder is slow." **Fix:** add a counter (frames queued − frames
presented) to the `Stats` frame / logs. Moonlight exposes exactly this ("frame queue
delay", target < 16 ms). One instrumented run will *prove* A/B/G worked instead of
guessing.

---

## 4. What comparable products do (and which to copy)

- **Moonlight (the right reference for our decode side):** separate input/render threads;
  bounded output queue (`OUTPUT_BUFFER_QUEUE_LIMIT = 2`); per-SoC vendor low-latency keys;
  `releaseOutputBuffer(index, timestamp)`; exposes decode/queue/render latency metrics.
  Decode-to-Surface, hardware only. → drives categories A, B, C, D, H.
- **scrcpy (NOT a decode reference — it decodes on the *desktop* with FFmpeg):** its
  transferable lesson is display discipline — keep only the **latest** frame, render ASAP,
  network on a separate thread. → reinforces A, D.
- **Parsec:** "no buffers of any kind on video"; priority **latency > frame rate >
  quality**; when behind, **drop bitrate, never grow a queue**; congestion *predicted*
  before queues form. For a fixed-rate USB link this is even more applicable. → reinforces
  A, E, F.
- **WebRTC:** `playout-delay` extension with **min=max=0** for gaming/remote-desktop;
  drops frames *before* decode under pressure; on loss must **wait for / request a
  keyframe and drop everything in between** (PLI/FIR). → drives F, validates input-side
  drop-to-keyframe in A.
- **General (it-jim, getstream):** "never build an unlimited buffer"; multithreaded
  assembly line pays *max* not *sum*; GStreamer `queue leaky=2` + `sync=false`; "prioritize
  the most recent data over complete data." → the textbook description of our bug and fix.

### Reference numbers to aim at (Moonlight, 1080p)
decode ≈ 4 ms · frame-queue delay ≈ 0 ms (target < 16 ms) · render ≈ 1 ms + vsync.
Our `send→arrive` (~0.8 ms) and tuned host (~50 ms total, improvable) are already in range;
the entire gap to 50 ms is the input-queue backlog.

---

## 5. Recommended order of attack
1. **H** (add in-flight depth counter) — so the next run is evidence, not a guess.
2. **A** (bound input queue + input-side drop-to-keyframe) — attacks the actual stage.
3. **B** (Exynos vendor key + confirm HW decoder) — keeps depth shallow at the source.
4. **G** (drain startup burst) — removes the primed initial depth.
5. Re-measure. If < 50 ms → done for PR #24. If still gated by per-frame decode or vsync:
6. **D** then **C** (vsync decouple, then full thread split) — the replaceable-later ideal.
7. **E/F** (intra-refresh / on-demand keyframes) — polish + bound resync cost.

## 6. Result after input pacing (items A + B + G + H) — measured 2026-06-05

Implemented the `InputPacer` (item A), the Exynos vendor low-latency key (B), startup-burst
handling via the depth cap (G), and depth instrumentation (H). Live device run (Pixel 6a,
M1, 2400×1080@60, offset −152 µs / rtt 304 µs):

| stage | before (last round) | after pacing | verdict |
|---|---|---|---|
| send→arrive (USB) | ~0.8 ms | ~0.3 ms | fine |
| **arrive→decode** | **~290–570 ms p50, growing (dominant)** | **~10 ms p50, ~14 ms p95, stable** | **SOLVED — ~50× reduction** |
| decode→present | ~13 ms | ~12 ms (one vsync) | fine |
| **capture→encode (host)** | ~30 ms p50 stable | **31 → 105 ms p50, GROWING; p95 405 ms, max 538 ms** | **NEW bottleneck** |
| encode→send (host) | ~18–25 ms | ~22 ms p50 | next-worst |
| GLASS→GLASS | ~490–830 ms p50 | best **80 ms p50** mid-run, drifting to ~158 ms | ~6× better; not yet <50 ms |

**Conclusion: the root-cause analysis was correct — the phone WAS the bottleneck, and items
A+B fixed it.** `arrive→decode` collapsed from ~500 ms (growing) to ~10 ms (stable). Phone
total is now ~22 ms. The phone-side pacer/coalesce drop counts stayed near zero in steady
state, confirming the decoder genuinely keeps up rather than being starved.

**The bottleneck moved upstream to the host `capture→encode` stage** — and it is the *same
bug class*, one stage up: the SCK capture delegate
([`p5_stream.rs:113–176`](../../../crates/macos-host/src/bin/p5_stream.rs)) stamps
`capture_us` then submits **every** 60 fps frame to VideoToolbox via the **async**
`encode_frame_with_output_handler` with **no bound on in-flight encodes**. When VT can't
sustain 60 fps (high-motion content, large frames), submitted-but-unencoded frames queue
*inside VideoToolbox*; `capture→encode` measures capture→handler-fire, so it grows
monotonically — exactly the bufferbloat signature we just removed on the phone. The existing
host coalesce can't help: it runs on the encoded-frame channel, *downstream* of where this
latency is incurred. This was masked in the last round because the slow phone backpressured
USB → capture, throttling the encoder for free; with the phone fast, the encoder is now fed
flat-out and its own queue is exposed.

### Next step (item I — host-side encoder pacing, mirrors item A)
Bound VideoToolbox's in-flight frames in the capture delegate: track submitted − completed;
if in-flight exceeds a small cap (1–2), **drop the SCK frame instead of submitting it**
(a live mirror prefers the freshest frame). This is the host analogue of `InputPacer`. The
`encode→send` ~22 ms is the next-worst stage and is a secondary target (likely the per-frame
USB write + framing; revisit after capture→encode). See category **E** for the IDR-bubble
angle (a 1 s GOP full IDR is a large frame that can spike both encode and transfer).

## 7. Result after non-blocking USB writes (PR 1 / GH #27) — measured 2026-06-07

PR 1 removed the per-frame **blocking** `flush()` from the host streaming send path: `AoaWriteHalf`
ended each frame with nusb's non-waiting `submit()` instead, relying on the depth-4 transfer ring
for backpressure. **Measured on device (M1 + Pixel 6a, 2400×1080@60, offset −12.36 s / rtt 430 µs):**

| stage | baseline | PR 1 light-load (n=5) | PR 1 steady-state (n=594) | verdict |
|---|---|---|---|---|
| capture→encode | ~11 ms p50 (spiky) | 10.2 p50 / 57.9 p95 | 10.9 p50 / **84 p95 / 150 max** | jitter tail (separate) |
| **encode→send** | **~22 ms p50** | 0.15 p50 | **0.23 p50** | collapsed as predicted ✅ |
| **send→arrive** | ~0.8 ms | 1.0 | **41.8 p50 / 101 p95** | **GREW — bufferbloat** ⬆ |
| arrive→decode | ~10 ms p50 | 9.1 | 12.0 p50 (425 max startup) | fine |
| decode→present | ~12 ms | 12.1 | 12.7 p50 | one vsync (floor) |
| **GLASS→GLASS** | **~80 ms p50** | **39 p50** | **100 p50 / 192 p95 / 454 max** | **REGRESSED** ⬆ |

**Conclusion: the plan's premise was wrong, and the measurement disproves it.** The per-frame
`flush()` was **not ~18 ms of waste — it was load-bearing flow control.** A bulk-OUT transfer
*completes only when the phone pulls the bytes* off its bulk-IN endpoint (USB-level flow control),
so the old flush time **was the phone-pull latency**, and blocking on it held the wire queue at
**depth ≈ 1** (drained to zero between frames). Removing it (1) **relocated** the ~22 ms from
`encode→send` to `send→arrive` (we now stamp `send_done` before the transfer completes), and (2)
**added** a persistent **~3-frame standing queue (~45 ms)** in the nusb ring + kernel USB buffers —
invisible to *both* sides' drop-to-keyframe logic (`dropped frames` stayed **0** the whole run).
Textbook standing-queue **bufferbloat**, and a direct violation of the prime directive ("never add
unbounded/invisible buffering on the hot path").

**Tells in the data:** `send→arrive` climbed `1 → 31 → 47 ms` and **plateaued** at ~45 ms (a full,
standing buffer — not unbounded), with `dropped = 0` throughout. The pretty 39 ms in the first
report was the **empty-buffer transient** before the queue filled. There is **no host-side free
lunch**: `num_transfers = 1` just recreates the blocking-flush depth-1 behavior. The ~22 ms is
fundamentally the **phone-pull rate**.

### What the floor analysis says is achievable

The light-load transient (39 ms, before any queue stood) ≈ the **sum of the per-stage floors**:
capture ~11 + wire ~3 + arrive→decode ~12 + decode→present ~13 ≈ **~39 ms**. So **~40 ms steady
state is reachable** — *if every queue is kept shallow*. The job is queue discipline, not shaving
any single stage.

### Revised strategy (supersedes the original PR ordering)

1. **Revert PR 1.** `flush()` is the correct shallow-queue gate; it restores the ~80 ms baseline.
   Host-side pipelining is a **dead end on its own**.
2. **PR 5 (phone RX thread) → promoted to #1.** The dominant steady-state cost is the **phone
   pull** (~22 ms, whether shown as `encode→send` or `send→arrive`). The *only* lever that shrinks
   it instead of relocating it is making the phone **drain USB continuously on a dedicated thread**,
   decoupled from decode/present, into a bounded **drop-to-keyframe** ring. That both (a) completes
   host transfers in ~wire time (~3 ms) so the 22 ms collapses *for real*, and (b) moves any
   standing queue to the phone's *app-level* ring where the existing `InputPacer` sheds it — not
   invisible kernel buffers. Keystone of getting under 50 ms.
3. **PR 1 revisited only after PR 5**, redesigned with **app-level in-flight accounting** (bounded
   outstanding-frame count / lightweight per-frame ack) so excess frames coalesce in the *visible*
   host channel, never kernel buffers. Safe only once the phone drains fast enough.
4. **PR 6 (IDR-bubble) rises in priority.** The periodic ~58 ms `capture→encode` spike (even at
   light load) is the **1 s full-IDR** — large, slow to encode *and* slow to transfer, so it also
   spikes `send→arrive`. Lengthen the GOP + on-demand keyframe back-channel (F) and/or intra-refresh
   (E). Fixes a tail visible in *two* stages.
5. **PR 2 (real-time scheduling)** for the remaining broad `capture→encode` p95/max tail.
6. **PR 3 / PR 4** (decoder hints, vsync) — polish; `arrive→decode` (~12 ms) and `decode→present`
   (~13 ms, one vsync) are already near their hardware floor.

**New ranked order: revert PR 1 → PR 5 → PR 6 → PR 2 → (PR 1 redux) → PR 3/4.**

## 8. Result after the drain-loop fix (GH #27) — measured 2026-06-07 — **TARGET MET**

After reverting PR 1, investigating why `decode→present` was stuck at ~12.7 ms regardless of the
present call uncovered the real bug: `drain_output` polled `dequeueOutputBuffer` with the full 10 ms
`DEQUEUE_TIMEOUT_US` on **every** iteration, so the trailing "any more buffers?" poll **blocked
~10 ms every frame** on the single decode thread. Fix: block only for the *first* buffer, poll the
rest with timeout 0. Measured on device (M1 + Pixel 6a, 2400×1080@60, offset −10.18 s / rtt 330 µs):

| stage | §7 (reverted baseline) | **after drain fix (n=593)** | verdict |
|---|---|---|---|
| capture→encode | ~12 ms p50 (p95 ~86) | 10.2 p50 / 12.5 p95 | fine (IDR max 60) |
| encode→send | ~0.5 ms p50 | 0.48 p50 | fine |
| **send→arrive** | **~30–42 ms p50** | **0.47 p50 / 1.2 p95** | **COLLAPSED** ✅ |
| arrive→decode | ~12 ms p50 | 20.1 p50 / 47 p95 | Tensor decode floor (now dominant) |
| **decode→present** | **~12.7 ms p50** | **1.79 p50** | **COLLAPSED** ✅ |
| **GLASS→GLASS** | **~110 ms p50** | **33.7 p50 / 60 p95** | **< 50 ms target MET** 🎯 |

**Root cause, finally:** the ~10 ms/frame trailing-dequeue block **compounded** — the single
read+decode thread fell ~10 ms behind every frame, so a ~30–42 ms standing queue built up in the
phone's USB receive buffer and surfaced as `send→arrive`. It was *also* mis-measured as
`decode→present` (the 10 ms sat between the decode-complete and present stamps). Removing it
collapsed **both** stages at once, with `dropped = 0` throughout (the phone genuinely keeps up).

**Honesty caveat:** PR 4 stamps `present_us` at the present *handoff*, not the actual scanout, so
the measured 33.7 ms under-counts the final photons by ~one vsync. **Real glass-to-glass ≈ 42–50 ms
— at the ~43 ms hardware floor.** Still a real ~2.5× improvement and at the floor.

**What's left (polish, not median):** `arrive→decode` (~20 ms p50) is now the largest stage — the
Tensor decode floor, near-irreducible. The p95 (~60 ms) and a one-time startup spike (first ~11
frames: `arrive→decode` ~135 ms while the decoder primes) are the remaining tails → PR 6 (IDR
bubble) / PR 7 (cold start). Per the plan's own rule, with p50 < 50 ms the remaining PRs are
jitter/robustness polish, not median wins.

**Meta-lesson:** a stage pinned at *exactly* a timeout value (12 ms ≈ the 10 ms `DEQUEUE_TIMEOUT_US`)
is almost always a blocking-poll artifact, not real latency. Two earlier rounds mis-attributed it to
the present (vsync coupling). Check the timeout constant before believing a stage.

## Sources
Moonlight `MediaCodecHelper.java` / `MediaCodecDecoderRenderer.java`; AOSP low-latency-media;
Android MediaCodec async ref; Apollo #1308 (Tensor decode latency); scrcpy develop.md /
rom1v blog; Parsec BUD protocol blog + parsec.app/technology; WebRTC playout-delay README,
webrtcforthecurious media-communication (NACK/PLI/FIR); it-jim real-time video pipelines;
getstream low-latency vision AI + WebRTC buffers; OBS x264 zerolatency; NXP intra-refresh.
