# RustScreen latency benchmarks — what to track & how to read it

RustScreen is a latency-critical real-time pipeline; **glass-to-glass latency is the metric.**
This doc defines the benchmarks to track, their targets, and how to capture them locally so any
proposed improvement can be verified against real numbers (not assumed) — the project's
measure-don't-guess rule (CLAUDE.md).

The pipeline:
**Mac capture → VideoToolbox H.264 encode → AOA/USB → phone MediaCodec decode → present.**

---

## 1. The metrics to track

The host prints a per-stage report every ~2 s while streaming (and once on exit). Each stage is
reported as **avg / p50 / p95 / max** in milliseconds. Track **p50** (typical feel) **and p95/max**
(the jitter that makes it "feel laggy").

| Metric | What it measures | Target (p50) | Hardware floor |
|---|---|---|---|
| `capture→encode` | SCK delivers a frame → VideoToolbox emits the encoded AU | ~10 ms | — (jitter from IDR + scheduling) |
| `encode→send` | encoded AU handed to the USB writer → write returns | ~1–22 ms¹ | — |
| `send→arrive` | host send done → phone reads the AU off USB | ~1 ms | ~wire time (~3 ms) |
| `arrive→decode` | phone queues input → decoder emits the frame | ~10–12 ms | Tensor decode floor |
| `decode→present` | decoded → handed to the compositor | ~1–13 ms² | one 60 Hz vsync (~8–16 ms) |
| **`GLASS→GLASS`** | **pixel changes on Mac → pixel on phone** | **< 50 ms** | **~43 ms** |

Plus three counters that explain the latency:
- **`dropped frames`** — host drop-to-keyframe shed load (intended under burst; *runaway* = under-provisioned).
- **`unmatched phone stats`** — host records evicted before the phone's stat arrived (should be ~0).
- **`anomalies`** — negative intervals from clock jitter, clamped to 0 (should be ~0).

¹ `encode→send` depends on the send model. With the per-frame blocking flush (current/baseline) it
reads ~22 ms — but that is the **phone-pull time** (a USB bulk-OUT transfer completes only when the
phone reads the bytes), *not* removable host-side work. See §4.
² `decode→present` drops to ~sub-ms once the present is non-blocking (PR 4 `releaseOutputBufferAtTime`);
the actual scanout is still ~one vsync later — that part is the panel floor.

### Targets & the floor (do not chase past this)

- **Goal: glass-to-glass < 50 ms p50, with p95 within ~1.5× of p50** (no visible jitter).
- **Floor ≈ 43 ms:** the phone's 60 Hz panel + the locked-60 Hz `CGVirtualDisplay` capture cadence
  are fixed by hardware. **Sub-~30 ms is not reachable** without new hardware. **Do not attempt**
  HEVC (anti-latency on Tensor), the 90 Hz panel unlock (firmware-locked), or >60 fps source
  (`CGVirtualDisplay` is hard-locked to 60 Hz on Apple Silicon).

---

## 2. How to capture a run

Run the host from a terminal that holds the macOS **Screen & System Audio Recording** grant
(a Claude-/IDE-launched process can't capture — black frames), with the Pixel plugged in and the
app open. Use a **release** build (debug materially worsens encode/copy latency):

```bash
cargo run --release -p macos-host --features "live-capture live-usb" --bin p5_stream
```

Capture **two scenes** each run:
- **Light motion** (move the cursor on the external display) → the **p50** you can live with.
- **Continuous window drag** (~10 s of sustained motion) → the **p95/max tail** (jitter).

On the phone side, the per-frame drop counter is in logcat:
```bash
adb logcat -s RustScreen:* | grep -i "input-dropped"
```

### The one-line summary

Each report ends with a grep-friendly line:

```
SUMMARY G2G p50=74.3 p95=226.4 | cap→enc 10.8 | enc→snd 0.2 | snd→arr 31.5 | arr→dec 12.2 | dec→pres 13.6 | drop=0 unmatched=0 (n=79)
```

`grep SUMMARY ~/.rustscreen/rustscreen.log` (or the `p5_stream` stdout) gives a quick run history.

---

## 3. Accumulate a diffable log across improvements

Set `RUSTSCREEN_LATENCY_CSV` to append every report (periodic + final) as a CSV row, so you can
compare a baseline run against a proposed-improvement run:

```bash
RUSTSCREEN_LATENCY_CSV=~/rustscreen-bench/baseline.csv \
  cargo run --release -p macos-host --features "live-capture live-usb" --bin p5_stream
# …apply a change, rebuild, then:
RUSTSCREEN_LATENCY_CSV=~/rustscreen-bench/change.csv \
  cargo run --release -p macos-host --features "live-capture live-usb" --bin p5_stream
```

Columns (all stage times in **ms**):
```
elapsed_s,n,g2g_p50,g2g_p95,g2g_max,cap_enc_p50,cap_enc_p95,enc_snd_p50,enc_snd_p95,
snd_arr_p50,snd_arr_p95,arr_dec_p50,arr_dec_p95,dec_pres_p50,dec_pres_p95,dropped,unmatched,anomalies
```

`elapsed_s` orders the rows; the **last row of each file** is the cumulative session result. Compare
the steady-state rows (after the queues fill — see §4), not the first row.

---

## 4. How to read the numbers (lessons learned)

**Compare steady state, not the transient.** The first one or two reports are captured before the
pipeline's buffers fill — they read artificially low. Always judge a change by the **steady-state**
rows under sustained motion, and by **p95/max**, not just the best p50.

**The standing-queue tell.** On a lossless USB link, *an unbounded/invisible queue at any stage is
pure latency.* The signature: a stage's p50 **climbs over the run and then plateaus** while
`dropped = 0` (nothing is shedding it). We hit exactly this trying to pipeline the USB writer
(PR 1): `encode→send` collapsed to ~0.2 ms but `send→arrive` grew `1 → 31 → 47 ms` and plateaued —
a ~3-frame standing queue in USB/kernel buffers, and glass-to-glass got *worse* (~80 → ~100 ms).
**If `snd→arr` (or any stage) climbs-and-plateaus with `drop=0`, you've moved the queue, not removed
it.** Full write-up: `docs/superpowers/research/2026-06-05-glass-to-glass-latency-root-cause.md` §7.

**Where the time really is.** `encode→send` ≈ 22 ms is the **phone pulling bytes off USB**, not
host work — it only shrinks if the phone drains USB faster (decouple the phone's read from its
decode/present). That's why the plan order is: non-blocking present (PR 4) → phone RX thread (PR 5)
→ IDR-bubble (PR 6) → real-time scheduling (PR 2). See
`docs/superpowers/plans/2026-06-07-native-feel-latency-multi-pr-plan.md`.

**A change is only good if** the stage it targets improves **and** glass-to-glass p50/p95 improves
**and** no other stage climbs-and-plateaus to absorb the win. Record before/after CSVs and check all
three.
