# Design: Phone Presents as a Real, Arrangeable External Display (Ladder Item 2 / PR #23)

> **Status:** Cable-free slice **IMPLEMENTED** (2026-06-04, branch
> `feat/p2-arrangeable-display`) for ROADMAP ladder **item 2** (phases P2→P5/P7).
> `DisplayConfig` + `hidpi_modes()` + `arrangement_origin()` are TDD'd green in CI (10
> tests); the `with_config`/`arrange`/`Drop` adapter compiles behind the macOS gate and
> `p5_stream` is wired. **Hands-on-Mac eyeball pending** (does it appear named/arrangeable/
> HiDPI in System Settings; does macOS remember its position across a reconnect).

## 1. Goal

After this work, the Pixel sits in **System Settings ▸ Displays** as a named monitor the
user can arrange (choose which side), it keeps a **stable identity** so macOS remembers
that position across reconnects, it offers a **HiDPI scaling option** (D6) so the desktop
renders crisp on the phone's high-DPI panel, and it is **removed cleanly on disconnect**
so the Mac desktop reflows.

All of this lives in the existing `cg-virtual-display` crate (item 2 says "same crate" —
coordinate with the merged objc2 work, PR #18). Item 2 is largely *verification + polish
on capability that already exists* (P2 already creates a named, arrangeable display); the
net-new substance is **HiDPI scaled modes** and **default-side placement**.

## 2. Scope split (cable-free vs. hands-on-Mac)

Consistent with how P3/P4 shipped: the **pure logic is TDD'd and green in CI** (the macOS
runner), the **private-API msg-send adapter compiles behind the existing macOS gate**, and
the **eyeball verification** (does it actually appear named/arrangeable/HiDPI in System
Settings, and does macOS remember its position across a reconnect) is a documented
hands-on-Mac follow-up — it cannot run in CI because it needs a real display + the private
CoreGraphics API + a human looking at the screen.

| In this PR (cable-free) | Deferred (hands-on-Mac) |
|---|---|
| `DisplayConfig` builder + defaults (pure, TDD) | Confirm the display appears as "RustScreen" |
| `hidpi_modes()` mode-list construction (pure, TDD) | Confirm the HiDPI scaling slider works in Settings |
| `arrangement_origin()` placement math (pure, TDD) | Confirm default side + macOS remembers manual rearrange |
| `with_config()` / `arrange()` msg-send adapter (compiles, macOS-gated) | Confirm clean teardown reflows the desktop on disconnect |
| `p5_stream` wired to build a `DisplayConfig` | — |

## 3. Approach (chosen: A — config/builder + pure helpers behind a backward-compatible `new`)

The real logic lives in **pure functions** so it is unit-testable in CI; the unsafe
msg-sends become thin consumers of those functions' outputs. `VirtualDisplay::new(w, h,
refresh)` stays as a thin wrapper over `with_config(DisplayConfig::new(w, h, refresh))`, so
every existing caller (`p3_capture`, `p3_encode`, `p3_probe`, `p5_stream`) compiles
untouched and the defaults preserve today's stable identity.

Rejected alternatives: **B** (knobs straight on `new` / free `arrange()` fn) tangles
placement + HiDPI logic into the unsafe paths, losing CI-testability; **C** (separate
arrangement crate/module) adds a crate boundary item 2 explicitly says to avoid.

## 4. Components

### 4.1 `DisplayConfig` (pure data + builder)
A plain config struct carrying everything the descriptor/settings need:
- `width`, `height`, `refresh` — as today.
- `name: String` — default `"RustScreen"`.
- **Identity** — `vendor_id`, `product_id`, `serial`, defaulted to the *current* constants
  (`0x726D` / `0x0001` / `0x0001`). Keeping these constant is what makes macOS remember the
  arrangement; exposing them documents that contract instead of burying it in msg-sends.
- `hidpi: bool` — default `true` for the live path (D6). `new()` legacy wrapper keeps `false`
  so existing spike bins are byte-identical to today.
- `arrangement: Arrangement` — `None` (let the user/last-remembered position win) or
  `Side(Side)` where `Side ∈ {Left, Right, Above, Below}`.

Builder methods (`.with_hidpi(bool)`, `.arranged(Side)`, `.named(&str)`, `.identity(...)`)
return `Self` for ergonomic construction. The builder itself is infallible; dimension
validation happens in `with_config`, where a zero dimension maps to
`VirtualDisplayError::CreationFailed` (no new error variant needed).

### 4.2 `hidpi_modes(native_w, native_h, refresh, hidpi) -> Vec<ModeSpec>` (pure)
The D6 deliverable. Produces the ordered, de-duplicated list of `CGVirtualDisplayMode`
specs to register on the settings object:
- `hidpi == false` → exactly one mode: the native `(native_w, native_h, refresh)` (today's
  behavior).
- `hidpi == true` → the native mode **plus** a 2× scaled mode so macOS exposes a
  "Larger Text … More Space" scaling slider; `setHiDPI: 1` tells CoreGraphics the modes are
  Retina-class. Ordering and de-dup are deterministic and unit-tested. `ModeSpec` is a pure
  `{ width, height, refresh }` triple the adapter turns into real mode objects.

> The *exact* macOS interpretation of HiDPI modes (logical vs. backing resolution) is
> verified by eyeball on the Mac; the **list-construction logic** (which specs, what order,
> no duplicates, native always present) is what we TDD here.

### 4.3 `arrangement_origin(main_bounds, vdisp_size, side) -> (i32, i32)` (pure)
Given the main display's bounds (`x, y, w, h`), the virtual display's pixel size, and a
`Side`, returns the global-space top-left origin to pass to `CGConfigureDisplayOrigin`:
- `Right` → `(main.x + main.w, main.y)`
- `Left`  → `(main.x - vdisp_w, main.y)`
- `Above` → `(main.x, main.y - vdisp_h)`
- `Below` → `(main.x, main.y + main.h)`
Boundary cases (negative origins for Left/Above, non-zero main origin) are unit-tested.

### 4.4 macOS adapter (private-API msg-sends; macOS-gated, eyeball-verified)
- `VirtualDisplay::with_config(&DisplayConfig)` — drives descriptor identity + name from the
  config, registers `hidpi_modes(...)` via `setModes:`, sets `setHiDPI: config.hidpi as u32`.
- `VirtualDisplay::new(w, h, refresh)` → `with_config(DisplayConfig::new(w, h, refresh))`
  (legacy defaults: HiDPI off, `Arrangement::None`). Backward-compatible.
- `arrange(side)` — wraps `CGConfigureDisplayOrigin` inside
  `CGBeginDisplayConfiguration`/`CGCompleteDisplayConfiguration`, sourcing main-display
  bounds from `CGDisplayBounds(CGMainDisplayID())` and the origin from
  `arrangement_origin(...)`. Called from `with_config` when `arrangement == Side(..)`.
- Explicit `Drop` with a log line so "removed on disconnect" is observable. The existing
  `drop(vdisplay)` at `crates/macos-host/src/bin/p5_stream.rs:412` already reflows the
  desktop; this just makes the teardown legible.

### 4.5 Wiring
`p5_stream` constructs `DisplayConfig::new(W, H, 60.0).with_hidpi(true).arranged(Side::Right)`
instead of the bare `VirtualDisplay::new(W, H, 60.0)`. No other caller changes.

## 5. Data flow

```
DisplayConfig ──▶ hidpi_modes() ──▶ Vec<ModeSpec> ─┐
              └─▶ identity/name ───────────────────┤──▶ with_config() msg-sends ──▶ live display
                                                   │
CGMainDisplayID()+CGDisplayBounds ─▶ arrangement_origin() ─▶ (x,y) ─▶ arrange() (CGConfigureDisplayOrigin)
                                                                                         │
                                                              session end / disconnect ──▶ drop(VirtualDisplay) ─▶ desktop reflows
```

## 6. Error handling
- Zero/invalid dimensions → `VirtualDisplayError::CreationFailed` (existing variant; no new
  error surface needed for the pure path).
- A failed `applySettings:` or absent private class → `CreationFailed`, as today.
- `arrange()` failures (e.g. `CGCompleteDisplayConfiguration` returns non-zero) are
  **non-fatal**: log a warning and keep the display alive at its default macOS position —
  placement is a convenience, not a correctness requirement (identity still lets macOS
  remember a manual arrangement). A new `VirtualDisplayError` variant is *not* introduced for
  this; arrange returns its own small `Result` logged at the call site.

## 7. Testing
- **CI (pure, TDD):** `DisplayConfig` defaults + builder; `hidpi_modes()` (native-only when
  off; native+2× deduped/ordered when on); `arrangement_origin()` for all four sides incl.
  boundary/offset cases. Target: clippy `-D warnings` clean, fmt clean, all green on the
  macOS CI runner.
- **Deferred (hands-on-Mac eyeball, documented in ROADMAP):** appears as "RustScreen";
  HiDPI scaling slider present and crisp; default side correct and macOS remembers a manual
  rearrange across reconnect; disconnect reflows the desktop.

## 8. Out of scope (YAGNI)
- Per-session dynamic re-arrangement UI / menu-bar controls (that's item 7, #28).
- Multiple simultaneous virtual displays.
- Resolution/orientation switching at runtime (item 8, #29).
- Cursor compositing (item 3, #24) and touch (item 4, #25) — separate ladder items.

## 9. ROADMAP impact
Item 2 row stays open until the hands-on-Mac eyeball passes; this PR lands the cable-free
slice and updates the item-2 notes to record what's proven-in-CI vs. eyeball-deferred,
mirroring the P3/P4 honesty pattern.
