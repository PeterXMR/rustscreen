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

/// Full configuration for a virtual display. Build with [`DisplayConfig::new`] + `with_*`.
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

    /// Enable/disable HiDPI scaled modes (D6).
    pub fn with_hidpi(mut self, hidpi: bool) -> Self {
        self.hidpi = hidpi;
        self
    }

    /// Place the display on `side` of the main display on first connect.
    pub fn arranged(mut self, side: Side) -> Self {
        self.arrangement = Arrangement::Side(side);
        self
    }

    /// Override the display name shown in System Settings.
    pub fn named(mut self, name: &str) -> Self {
        self.name = name.to_string();
        self
    }
}

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
    let native = ModeSpec {
        width,
        height,
        refresh,
    };
    if !hidpi {
        return vec![native];
    }
    let mut modes = vec![native.clone()];
    let scaled = ModeSpec {
        width: width / 2,
        height: height / 2,
        refresh,
    };
    if scaled.width > 0 && scaled.height > 0 && scaled != native {
        modes.push(scaled);
    }
    modes
}

/// Compute the global-space top-left origin for the virtual display so it sits on the
/// given `side` of the main display. `vdisp_size` is the virtual display's pixel size.
/// The result is passed to `CGConfigureDisplayOrigin`.
pub fn arrangement_origin(main: DisplayBounds, vdisp_size: (i32, i32), side: Side) -> (i32, i32) {
    let (vw, vh) = vdisp_size;
    match side {
        Side::Right => (main.x + main.width, main.y),
        Side::Left => (main.x - vw, main.y),
        Side::Above => (main.x, main.y - vh),
        Side::Below => (main.x, main.y + main.height),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_uses_legacy_defaults() {
        let c = DisplayConfig::new(2400, 1080, 60.0);
        assert_eq!(c.name, "RustScreen");
        assert_eq!(
            (c.vendor_id, c.product_id, c.serial),
            (0x726D, 0x0001, 0x0001)
        );
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

    #[test]
    fn hidpi_off_yields_only_native_mode() {
        let modes = hidpi_modes(2400, 1080, 60.0, false);
        assert_eq!(
            modes,
            vec![ModeSpec {
                width: 2400,
                height: 1080,
                refresh: 60.0
            }]
        );
    }

    #[test]
    fn hidpi_on_adds_a_scaled_mode_native_first() {
        let modes = hidpi_modes(2400, 1080, 60.0, true);
        assert_eq!(modes.len(), 2, "native + one 2x-scaled mode");
        assert_eq!(
            modes[0],
            ModeSpec {
                width: 2400,
                height: 1080,
                refresh: 60.0
            }
        );
        assert_eq!(
            modes[1],
            ModeSpec {
                width: 1200,
                height: 540,
                refresh: 60.0
            }
        );
    }

    #[test]
    fn hidpi_on_dedups_when_scaled_equals_native() {
        // 0/1-px degenerate input can't produce a distinct half; never emit a dup.
        let modes = hidpi_modes(1, 1, 60.0, true);
        assert_eq!(
            modes,
            vec![ModeSpec {
                width: 1,
                height: 1,
                refresh: 60.0
            }]
        );
    }

    fn main_at(x: i32, y: i32, w: i32, h: i32) -> DisplayBounds {
        DisplayBounds {
            x,
            y,
            width: w,
            height: h,
        }
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
}
