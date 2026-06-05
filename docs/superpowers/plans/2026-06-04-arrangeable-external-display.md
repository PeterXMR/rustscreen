# Arrangeable External Display (Ladder Item 2 / PR #23) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make the Pixel present as a named, arrangeable, HiDPI external display that macOS remembers and that tears down cleanly on disconnect — landing the cable-free slice (pure logic TDD'd in CI + macOS-gated msg-send adapter compiling) and deferring the System Settings eyeball.

**Architecture:** Push the real logic into pure functions in `cg-virtual-display` — a `DisplayConfig` builder, `hidpi_modes()` (D6 mode-list), and `arrangement_origin()` (placement math) — all unit-tested on the macOS CI runner. The private-`CGVirtualDisplay` msg-sends and `CGConfigureDisplayOrigin` FFI become thin consumers of those outputs. `VirtualDisplay::new(w,h,refresh)` stays backward-compatible as a wrapper over `with_config()`.

**Tech Stack:** Rust 2021 · `objc2` family (existing crate deps) · inline `extern "C"` CoreGraphics FFI (matching the existing `CGGetActiveDisplayList` block) · no new dependencies.

---

## File Structure

- `crates/cg-virtual-display/src/config.rs` — **new**: pure types (`DisplayConfig`, `Side`, `Arrangement`, `ModeSpec`, `DisplayBounds`) + builder + `hidpi_modes()` + `arrangement_origin()`. All `#[cfg]`-free pure Rust; unit tests live here.
- `crates/cg-virtual-display/src/lib.rs` — **modify**: `mod config; pub use config::*;`; refactor `new_inner` → `with_config`; add `new()` wrapper; add `arrange()` FFI + call; add `Drop` log.
- `crates/macos-host/src/bin/p5_stream.rs:256` — **modify**: build a `DisplayConfig` (HiDPI on, `Side::Right`) instead of bare `new`.
- `.planning/ROADMAP.md` — **modify**: update item-2 row + status with proven-in-CI vs eyeball-deferred.
- `docs/superpowers/specs/2026-06-04-arrangeable-external-display-design.md` — **modify**: flip status to implemented.

> **Note on test execution:** the crate is `#![cfg(target_os = "macos")]`, so `cargo test -p cg-virtual-display` runs on the macOS CI runner. The pure helpers (Tasks 1–3) need no private API and are fully TDD'd. The adapter (Tasks 4–5) drives a private, headless-unfriendly API, so those tasks verify **compile + clippy + the legacy `new()` still type-checks**, not runtime creation — the runtime behavior is the deferred eyeball.

---

## Task 1: Config types + builder (pure)

**Files:**
- Create: `crates/cg-virtual-display/src/config.rs`
- Modify: `crates/cg-virtual-display/src/lib.rs` (add `mod config; pub use config::*;` after the `#![cfg(...)]` line)

- [ ] **Step 1: Write the failing tests**

In `crates/cg-virtual-display/src/config.rs`:

```rust
//! Pure, dependency-free configuration + layout logic for the virtual display.
//!
//! Everything here is plain Rust (no `objc2`, no FFI) so it is unit-tested on the
//! macOS CI runner. The private-API adapter in `lib.rs` consumes these outputs.

/// One display mode to register, in **pixel** dimensions.
#[derive(Debug, Clone, PartialEq)]
pub struct ModeSpec {
    pub width: u32,
    pub height: u32,
    pub refresh: f64,
}

/// Which side of the main display to place the virtual display on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}

/// Where to place the virtual display on first connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    /// Leave placement to macOS (restores the last remembered position).
    None,
    /// Place on the given side of the main display.
    Side(Side),
}

/// Main-display bounds in global points, as reported by `CGDisplayBounds`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DisplayBounds {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

/// Full configuration for a virtual display. Build with `DisplayConfig::new` + `with_*`.
#[derive(Debug, Clone, PartialEq)]
pub struct DisplayConfig {
    pub width: u32,
    pub height: u32,
    pub refresh: f64,
    pub name: String,
    pub vendor_id: u32,
    pub product_id: u32,
    pub serial: u32,
    pub hidpi: bool,
    pub arrangement: Arrangement,
}

impl DisplayConfig {
    /// A config with the legacy defaults: the original stable identity, HiDPI **off**,
    /// and no programmatic arrangement — byte-identical to the pre-item-2 `new`.
    pub fn new(width: u32, height: u32, refresh: f64) -> Self {
        Self {
            width,
            height,
            refresh,
            name: "RustScreen".to_string(),
            // Stable identity → macOS remembers the arrangement across reconnects.
            vendor_id: 0x726D,
            product_id: 0x0001,
            serial: 0x0001,
            hidpi: false,
            arrangement: Arrangement::None,
        }
    }

    pub fn with_hidpi(mut self, hidpi: bool) -> Self {
        self.hidpi = hidpi;
        self
    }

    pub fn arranged(mut self, side: Side) -> Self {
        self.arrangement = Arrangement::Side(side);
        self
    }

    pub fn named(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_legacy_defaults() {
        let c = DisplayConfig::new(2400, 1080, 60.0);
        assert_eq!(c.name, "RustScreen");
        assert_eq!((c.vendor_id, c.product_id, c.serial), (0x726D, 0x0001, 0x0001));
        assert!(!c.hidpi, "legacy default must keep HiDPI off");
        assert_eq!(c.arrangement, Arrangement::None);
    }

    #[test]
    fn builders_chain_and_override() {
        let c = DisplayConfig::new(2400, 1080, 60.0)
            .with_hidpi(true)
            .arranged(Side::Right)
            .named("Pixel");
        assert!(c.hidpi);
        assert_eq!(c.arrangement, Arrangement::Side(Side::Right));
        assert_eq!(c.name, "Pixel");
        // dimensions untouched by builders
        assert_eq!((c.width, c.height, c.refresh), (2400, 1080, 60.0));
    }
}
```

- [ ] **Step 2: Wire the module and run the tests to verify they fail**

Add to `crates/cg-virtual-display/src/lib.rs` immediately after the `#![cfg(target_os = "macos")]` line:

```rust
mod config;
pub use config::*;
```

Run: `cargo test -p cg-virtual-display config::tests -- --nocapture`
Expected: COMPILES then PASS (these are construction-only; if you mistyped a field they fail to compile). To see a real red first, temporarily change `hidpi: false` to `hidpi: true` in `new`, watch `new_uses_legacy_defaults` FAIL, then revert.

- [ ] **Step 3: Run the full crate tests + lints**

Run: `cargo test -p cg-virtual-display && cargo clippy -p cg-virtual-display --all-targets -- -D warnings && cargo fmt -p cg-virtual-display -- --check`
Expected: all PASS / clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cg-virtual-display/src/config.rs crates/cg-virtual-display/src/lib.rs
git commit -m "feat(P2): DisplayConfig builder for virtual display (item 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 2: `hidpi_modes()` — the D6 mode-list (pure)

**Files:**
- Modify: `crates/cg-virtual-display/src/config.rs`

- [ ] **Step 1: Write the failing tests** (append to the `tests` module in `config.rs`)

```rust
    #[test]
    fn hidpi_off_yields_only_native_mode() {
        let modes = hidpi_modes(2400, 1080, 60.0, false);
        assert_eq!(modes, vec![ModeSpec { width: 2400, height: 1080, refresh: 60.0 }]);
    }

    #[test]
    fn hidpi_on_adds_a_scaled_mode_native_first() {
        let modes = hidpi_modes(2400, 1080, 60.0, true);
        assert_eq!(modes.len(), 2, "native + one 2x-scaled mode");
        assert_eq!(modes[0], ModeSpec { width: 2400, height: 1080, refresh: 60.0 });
        assert_eq!(modes[1], ModeSpec { width: 1200, height: 540, refresh: 60.0 });
    }

    #[test]
    fn hidpi_on_dedups_when_scaled_equals_native() {
        // 0/1-px degenerate input can't produce a distinct half; never emit a dup.
        let modes = hidpi_modes(1, 1, 60.0, true);
        assert_eq!(modes, vec![ModeSpec { width: 1, height: 1, refresh: 60.0 }]);
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-virtual-display hidpi_modes`
Expected: FAIL — `cannot find function hidpi_modes`.

- [ ] **Step 3: Implement** (add to `config.rs`, above the `tests` module)

```rust
/// Build the ordered, de-duplicated list of pixel modes to register on the virtual
/// display's settings object.
///
/// - `hidpi == false` → exactly the native pixel mode (legacy behavior).
/// - `hidpi == true`  → the native mode **plus** a half-resolution mode; combined with
///   `setHiDPI: 1` this surfaces a macOS scaling slider ("Larger Text … More Space").
///
/// The half-resolution policy is a deterministic starting point validated by the on-Mac
/// eyeball; the *list construction* (native always first, no duplicates, no degenerate
/// zero-size modes) is what this function guarantees and what the tests pin.
pub fn hidpi_modes(width: u32, height: u32, refresh: f64, hidpi: bool) -> Vec<ModeSpec> {
    let native = ModeSpec { width, height, refresh };
    if !hidpi {
        return vec![native];
    }
    let mut modes = vec![native.clone()];
    let scaled = ModeSpec { width: width / 2, height: height / 2, refresh };
    if scaled.width > 0 && scaled.height > 0 && scaled != native {
        modes.push(scaled);
    }
    modes
}
```

- [ ] **Step 4: Run to verify pass + lints**

Run: `cargo test -p cg-virtual-display && cargo clippy -p cg-virtual-display --all-targets -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cg-virtual-display/src/config.rs
git commit -m "feat(P2): hidpi_modes() mode-list construction (D6, item 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 3: `arrangement_origin()` — placement math (pure)

**Files:**
- Modify: `crates/cg-virtual-display/src/config.rs`

- [ ] **Step 1: Write the failing tests** (append to the `tests` module)

```rust
    fn main_at(x: i32, y: i32, w: i32, h: i32) -> DisplayBounds {
        DisplayBounds { x, y, width: w, height: h }
    }

    #[test]
    fn right_places_past_main_right_edge() {
        let o = arrangement_origin(main_at(0, 0, 1920, 1080), (2400, 1080), Side::Right);
        assert_eq!(o, (1920, 0));
    }

    #[test]
    fn left_places_negative_by_vdisp_width() {
        let o = arrangement_origin(main_at(0, 0, 1920, 1080), (2400, 1080), Side::Left);
        assert_eq!(o, (-2400, 0));
    }

    #[test]
    fn above_places_negative_by_vdisp_height() {
        let o = arrangement_origin(main_at(0, 0, 1920, 1080), (2400, 1080), Side::Above);
        assert_eq!(o, (0, -1080));
    }

    #[test]
    fn below_places_past_main_bottom_edge() {
        let o = arrangement_origin(main_at(0, 0, 1920, 1080), (2400, 1080), Side::Below);
        assert_eq!(o, (0, 1080));
    }

    #[test]
    fn respects_nonzero_main_origin() {
        let o = arrangement_origin(main_at(100, 50, 1920, 1080), (2400, 1080), Side::Right);
        assert_eq!(o, (2020, 50));
    }
```

- [ ] **Step 2: Run to verify failure**

Run: `cargo test -p cg-virtual-display arrangement_origin`
Expected: FAIL — `cannot find function arrangement_origin`.

- [ ] **Step 3: Implement** (add to `config.rs`, above the `tests` module)

```rust
/// Compute the global-space top-left origin for the virtual display so it sits on the
/// given `side` of the main display. `vdisp_size` is the virtual display's pixel size.
/// The result is passed to `CGConfigureDisplayOrigin`.
pub fn arrangement_origin(
    main: DisplayBounds,
    vdisp_size: (i32, i32),
    side: Side,
) -> (i32, i32) {
    let (vw, vh) = vdisp_size;
    match side {
        Side::Right => (main.x + main.width, main.y),
        Side::Left => (main.x - vw, main.y),
        Side::Above => (main.x, main.y - vh),
        Side::Below => (main.x, main.y + main.height),
    }
}
```

- [ ] **Step 4: Run to verify pass + lints**

Run: `cargo test -p cg-virtual-display && cargo clippy -p cg-virtual-display --all-targets -- -D warnings`
Expected: PASS / clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cg-virtual-display/src/config.rs
git commit -m "feat(P2): arrangement_origin() placement math (item 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 4: Adapter — `with_config()` + backward-compatible `new()`

**Files:**
- Modify: `crates/cg-virtual-display/src/lib.rs` (refactor `new`/`new_inner`)

> No runtime test (private headless-unfriendly API). The gate is: compiles, clippy clean, and the legacy `new()` keeps its exact signature so `p3_*`/`p5_stream` still build.

- [ ] **Step 1: Replace `new` + `new_inner` to read from `DisplayConfig`**

In `crates/cg-virtual-display/src/lib.rs`, change `VirtualDisplay::new` to delegate, and rewrite `new_inner` to take a `&DisplayConfig`. Replace the existing `impl VirtualDisplay { pub fn new(...) ... unsafe fn new_inner(...) ... }` head and the hardcoded descriptor lines:

```rust
impl VirtualDisplay {
    /// Create a virtual display of `width`x`height` pixels at `refresh` Hz, with the
    /// legacy defaults (stable identity, HiDPI off, no programmatic arrangement).
    pub fn new(width: u32, height: u32, refresh: f64) -> Result<Self, VirtualDisplayError> {
        Self::with_config(&DisplayConfig::new(width, height, refresh))
    }

    /// Create a virtual display from a full [`DisplayConfig`] (item 2: name, identity,
    /// HiDPI scaled modes, and optional default-side arrangement).
    pub fn with_config(cfg: &DisplayConfig) -> Result<Self, VirtualDisplayError> {
        if cfg.width == 0 || cfg.height == 0 {
            return Err(VirtualDisplayError::CreationFailed);
        }
        // SAFETY: see new_inner.
        unsafe { Self::new_inner(cfg) }
    }

    unsafe fn new_inner(cfg: &DisplayConfig) -> Result<Self, VirtualDisplayError> {
```

Then, inside `new_inner`, replace the hardcoded descriptor + mode + settings lines with config-driven ones:

- Name: `let name = NSString::from_str(&cfg.name);`
- Dimensions: use `cfg.width` / `cfg.height` everywhere `width`/`height` appeared.
- Physical size: `let size = CGSize::new(cfg.width as f64 * 0.231, cfg.height as f64 * 0.231);`
- Identity:
  ```rust
  let _: () = msg_send![&*desc, setProductID: cfg.product_id];
  let _: () = msg_send![&*desc, setVendorID: cfg.vendor_id];
  let _: () = msg_send![&*desc, setSerialNum: cfg.serial];
  ```
- Modes — replace the single-mode block with a loop over `hidpi_modes`:
  ```rust
  let specs = hidpi_modes(cfg.width, cfg.height, cfg.refresh, cfg.hidpi);
  let mut mode_objs: Vec<Retained<AnyObject>> = Vec::with_capacity(specs.len());
  for spec in &specs {
      let allocated: *mut AnyObject = msg_send![mode_cls, alloc];
      let obj: *mut AnyObject = msg_send![
          allocated,
          initWithWidth: spec.width,
          height: spec.height,
          refreshRate: spec.refresh,
      ];
      mode_objs.push(Retained::from_raw(obj).ok_or(VirtualDisplayError::CreationFailed)?);
  }
  let modes = NSArray::from_retained_slice(&mode_objs);
  let _: () = msg_send![&*settings, setModes: &*modes];
  let _: () = msg_send![&*settings, setHiDPI: cfg.hidpi as u32];
  ```
  (Delete the old single `mode` binding, the old `setModes:` with `from_ref(&mode)`, and the old `setHiDPI: 0u32` line.)
- Refresh references: replace any bare `refresh` with `cfg.refresh` where still used.

- [ ] **Step 2: Verify it compiles + the legacy callers still build**

Run: `cargo build -p cg-virtual-display && cargo build -p macos-host --bin p3_probe --features live-capture`
Expected: both compile. (`p3_probe` uses `VirtualDisplay::new` unchanged.)

- [ ] **Step 3: Clippy + fmt**

Run: `cargo clippy -p cg-virtual-display --all-targets -- -D warnings && cargo fmt -p cg-virtual-display -- --check`
Expected: clean.

- [ ] **Step 4: Commit**

```bash
git add crates/cg-virtual-display/src/lib.rs
git commit -m "feat(P2): VirtualDisplay::with_config drives identity + HiDPI modes (item 2)

new() stays a thin wrapper over with_config(DisplayConfig::new(..)), so every
existing caller is untouched.

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 5: `arrange()` via CG FFI + observable `Drop`

**Files:**
- Modify: `crates/cg-virtual-display/src/lib.rs`

- [ ] **Step 1: Add the CoreGraphics display-configuration FFI**

Add near the existing `CGGetActiveDisplayList` `extern "C"` block in `lib.rs` (top-level), a module-local FFI surface:

```rust
/// Minimal CoreGraphics display-configuration FFI (same inline-`extern "C"` style as
/// `active_display_count` below). Used to place the virtual display on a chosen side.
mod cg {
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGPoint {
        pub x: f64,
        pub y: f64,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGRect {
        pub origin: CGPoint,
        pub size: CGSize,
    }
    #[repr(C)]
    #[derive(Clone, Copy)]
    pub struct CGSize {
        pub width: f64,
        pub height: f64,
    }
    pub type CGDirectDisplayID = u32;
    pub type CGDisplayConfigRef = *mut core::ffi::c_void;

    extern "C" {
        pub fn CGMainDisplayID() -> CGDirectDisplayID;
        pub fn CGDisplayBounds(display: CGDirectDisplayID) -> CGRect;
        pub fn CGBeginDisplayConfiguration(config: *mut CGDisplayConfigRef) -> i32;
        pub fn CGConfigureDisplayOrigin(
            config: CGDisplayConfigRef,
            display: CGDirectDisplayID,
            x: i32,
            y: i32,
        ) -> i32;
        pub fn CGCompleteDisplayConfiguration(config: CGDisplayConfigRef, option: u32) -> i32;
    }
}
```

- [ ] **Step 2: Add `arrange()` and call it from `with_config`**

Add a method on `VirtualDisplay`:

```rust
    /// Place this display on `side` of the main display via `CGConfigureDisplayOrigin`.
    /// Non-fatal: on any CG error the display stays at its default macOS position.
    pub fn arrange(&self, side: Side) {
        // SAFETY: pure CG display-configuration transaction; display_id is this display's.
        unsafe {
            let main_id = cg::CGMainDisplayID();
            let b = cg::CGDisplayBounds(main_id);
            let main = DisplayBounds {
                x: b.origin.x as i32,
                y: b.origin.y as i32,
                width: b.size.width as i32,
                height: b.size.height as i32,
            };
            // Use the configured display's own bounds for its pixel size.
            let vb = cg::CGDisplayBounds(self.display_id);
            let vsize = (vb.size.width as i32, vb.size.height as i32);
            let (ox, oy) = arrangement_origin(main, vsize, side);

            let mut token: cg::CGDisplayConfigRef = core::ptr::null_mut();
            if cg::CGBeginDisplayConfiguration(&mut token) != 0 {
                eprintln!("cg-virtual-display: arrange: begin-config failed; leaving default position");
                return;
            }
            if cg::CGConfigureDisplayOrigin(token, self.display_id, ox, oy) != 0 {
                eprintln!("cg-virtual-display: arrange: set-origin failed; leaving default position");
                let _ = cg::CGCompleteDisplayConfiguration(token, 0); // 0 = kCGConfigureForAppOnly
                return;
            }
            if cg::CGCompleteDisplayConfiguration(token, 0) != 0 {
                eprintln!("cg-virtual-display: arrange: complete-config failed");
            }
        }
    }
```

Then, at the end of `new_inner`, just before `Ok(Self { ... })`, build the value and arrange if requested:

```rust
        let display = Self {
            _display: disp,
            display_id,
        };
        if let Arrangement::Side(side) = cfg.arrangement {
            display.arrange(side);
        }
        Ok(display)
```

(Replace the existing `Ok(Self { _display: disp, display_id })` tail.)

- [ ] **Step 3: Add an observable `Drop`**

Add after the `impl VirtualDisplay` block:

```rust
impl Drop for VirtualDisplay {
    fn drop(&mut self) {
        // The retained CGVirtualDisplay is released right after this, tearing the display
        // down so the desktop reflows. Log it so disconnect teardown is observable.
        eprintln!(
            "cg-virtual-display: removing virtual display {} (desktop will reflow)",
            self.display_id
        );
    }
}
```

- [ ] **Step 4: Compile + clippy + fmt**

Run: `cargo build -p cg-virtual-display && cargo clippy -p cg-virtual-display --all-targets -- -D warnings && cargo fmt -p cg-virtual-display -- --check`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/cg-virtual-display/src/lib.rs
git commit -m "feat(P2): arrange() via CGConfigureDisplayOrigin + observable Drop (item 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 6: Wire `p5_stream` to request HiDPI + default side

**Files:**
- Modify: `crates/macos-host/src/bin/p5_stream.rs:254-263`

- [ ] **Step 1: Swap the bare `new` for a `DisplayConfig`**

Replace (around line 256):

```rust
    let vdisplay = match VirtualDisplay::new(W as u32, H as u32, 60.0) {
```

with:

```rust
    use cg_virtual_display::{DisplayConfig, Side};
    let cfg = DisplayConfig::new(W as u32, H as u32, 60.0)
        .with_hidpi(true)
        .arranged(Side::Right);
    let vdisplay = match VirtualDisplay::with_config(&cfg) {
```

(The `use cg_virtual_display::VirtualDisplay;` already present at line 239 stays; add the new `use` next to it or inline as shown.)

- [ ] **Step 2: Build the live bin**

Run: `cargo build -p macos-host --bin p5_stream --features "live-capture live-usb"`
Expected: compiles.

- [ ] **Step 3: Clippy the whole workspace + fmt**

Run: `cargo clippy --workspace --all-targets -- -D warnings && cargo fmt --all -- --check`
Expected: clean. Then `cargo test --workspace` → all green (no regressions).

- [ ] **Step 4: Commit**

```bash
git add crates/macos-host/src/bin/p5_stream.rs
git commit -m "feat(P2): p5_stream requests HiDPI + right-side placement (item 2)

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 7: ROADMAP + spec status

**Files:**
- Modify: `.planning/ROADMAP.md` (item-2 ladder row + the P-status notes)
- Modify: `docs/superpowers/specs/2026-06-04-arrangeable-external-display-design.md` (status line)

- [ ] **Step 1: Update the spec status**

Change the spec's status line to: `> **Status:** Cable-free slice IMPLEMENTED (2026-06-04, branch feat/p2-arrangeable-display). Hands-on-Mac eyeball pending.`

- [ ] **Step 2: Update ROADMAP item 2**

In the Delivery PR Ladder table, update item 2's row to note the cable-free slice landed (DisplayConfig + hidpi_modes + arrangement_origin TDD'd; with_config/arrange adapter compiled; p5_stream wired) with the System Settings eyeball deferred — mirror the P3/P4 honesty wording. Add a short "Cable-free slice done; eyeball-deferred" note under the relevant status section.

- [ ] **Step 3: Commit**

```bash
git add .planning/ROADMAP.md docs/superpowers/specs/2026-06-04-arrangeable-external-display-design.md
git commit -m "docs(P2): record item-2 cable-free slice + eyeball-deferred status

Co-Authored-By: Claude Opus 4.8 <noreply@anthropic.com>"
```

---

## Task 8: Open PR #23

- [ ] **Step 1: Push the branch**

Run: `git push -u origin feat/p2-arrangeable-display`

- [ ] **Step 2: Open the PR**

Run `gh pr create` with title `feat(P2): phone presents as a real, arrangeable HiDPI external display (ladder item 2)` and a body summarizing: what the user gets, the cable-free vs eyeball-deferred split, the pure-helper TDD coverage, and that it depends on merged item 1 (#22). End the body with the Claude Code attribution line.

Expected: PR created against `main`; CI runs the workspace tests + clippy on the macOS runner.
