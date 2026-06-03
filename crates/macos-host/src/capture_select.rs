//! Capture-adapter selection — pure, cable-free decision logic (RESEARCH Pitfall 1).
//!
//! `CGVirtualDisplay` displays are frequently **absent** from
//! `SCShareableContent.displays` (Apple bug FB17797423), so RustScreen cannot assume
//! ScreenCaptureKit can see the P2 virtual display. The headline risk mitigation
//! (CONTEXT D4) is a co-equal `CGDisplayStream` fallback: at runtime we probe the
//! shareable display list and pick the SCK path only when the target display id is
//! actually present, otherwise we fall back.
//!
//! That decision is pure logic and is unit-tested here. The real shareable-display
//! list is hidden behind the tiny [`DisplaySource`] trait so no live ScreenCaptureKit
//! call is needed in tests; the Wave-B `SckCapturer` implements `DisplaySource` over
//! `SCShareableContent`. Matching is **strictly by display id, never by index**
//! (threat T-P3-03: capturing the wrong display).
//!
//! No macOS / objc2 crate is imported by the compiled code here — Wave A CI stays
//! cross-platform and dependency-free.

/// A source of the currently shareable display ids (e.g. ScreenCaptureKit's
/// `SCShareableContent.displays` mapped to their `CGDirectDisplayID`s).
///
/// Abstracted so the selection logic is testable with a fake and so the real
/// ScreenCaptureKit adapter (Wave B) can supply the live list without this module
/// depending on any macOS API.
pub trait DisplaySource {
    /// The `CGDirectDisplayID`s of every display ScreenCaptureKit currently reports as
    /// shareable. Empty when the TCC grant is missing or no display is visible.
    fn shareable_display_ids(&self) -> Vec<u32>;
}

/// Which capture adapter the spike should drive for the target display.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureBackend {
    /// ScreenCaptureKit (`SCStream`) — preferred when the target is shareable.
    ScreenCaptureKit,
    /// `CGDisplayStream` — co-equal fallback when SCK cannot see the target (D4).
    CgDisplayStream,
}

// Note: the "neither capture path works" keystone case (RESEARCH Pitfall 1.4) is a
// hands-on-Mac runtime escalation introduced with the Wave B adapters, not a branch of
// this pure selection logic — so no error type is defined here until it can be produced.

/// Choose the capture backend for `target_id` given the currently shareable displays.
///
/// Returns [`CaptureBackend::ScreenCaptureKit`] iff `target_id` appears in
/// `source.shareable_display_ids()` (matched **by id**, never by position), otherwise
/// [`CaptureBackend::CgDisplayStream`] — including when the shareable list is empty
/// (TCC not granted or the virtual display is invisible to SCK; CONTEXT D4).
pub fn select_backend(source: &dyn DisplaySource, target_id: u32) -> CaptureBackend {
    if source.shareable_display_ids().contains(&target_id) {
        CaptureBackend::ScreenCaptureKit
    } else {
        CaptureBackend::CgDisplayStream
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fake shareable-display list standing in for live `SCShareableContent`.
    struct FakeDisplaySource {
        ids: Vec<u32>,
    }
    impl DisplaySource for FakeDisplaySource {
        fn shareable_display_ids(&self) -> Vec<u32> {
            self.ids.clone()
        }
    }

    #[test]
    fn present_target_selects_screen_capture_kit() {
        let src = FakeDisplaySource { ids: vec![42] };
        assert_eq!(select_backend(&src, 42), CaptureBackend::ScreenCaptureKit);
    }

    #[test]
    fn absent_target_falls_back_to_cg_display_stream() {
        let src = FakeDisplaySource { ids: vec![1, 2, 3] };
        assert_eq!(select_backend(&src, 42), CaptureBackend::CgDisplayStream);
    }

    #[test]
    fn empty_list_falls_back_to_cg_display_stream() {
        let src = FakeDisplaySource { ids: vec![] };
        assert_eq!(select_backend(&src, 42), CaptureBackend::CgDisplayStream);
    }

    #[test]
    fn matches_by_id_among_multiple_displays_not_by_index() {
        // Target is present but NOT first — selection must match by id, never index.
        let src = FakeDisplaySource {
            ids: vec![7, 99, 42, 13],
        };
        assert_eq!(select_backend(&src, 42), CaptureBackend::ScreenCaptureKit);
        // A different non-present id with the same list still falls back, proving the
        // decision keys on id membership rather than the list being non-empty.
        assert_eq!(select_backend(&src, 100), CaptureBackend::CgDisplayStream);
    }
}
