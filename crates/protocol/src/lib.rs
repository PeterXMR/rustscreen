pub mod clock;
pub mod framing;
pub mod messages;
pub mod nal;

/// Maximum size, in bytes, of a single USB bulk transfer on the Mac↔Pixel AOA link.
///
/// This is the **single source of truth** for the transport's per-transfer granularity,
/// shared by both ends so they can never drift:
/// - the **host** (`macos-host` `aoa::AoaTransport`) sizes its `nusb` `EndpointWrite`/
///   `EndpointRead` buffers to this, so no single bulk OUT URB exceeds it;
/// - the **device** (`android-client` `AccessoryFdTransport`) sizes the `read()` it issues on
///   the accessory fd to this, so a whole host transfer fits in one read.
///
/// **Why this MUST stay one constant (load-bearing — guards a real, debugged hardware bug):**
/// on the Android `f_accessory` gadget the size passed to `read()` *bounds* the USB OUT
/// request, so if the host's per-transfer size ever exceeded the device's read size the gadget
/// would silently truncate every packet and deadlock the link. Tying both ends to this one
/// value makes that failure mode unrepresentable: raise it here and both move together.
pub const BULK_TRANSFER_SIZE: usize = 16 * 1024;

/// Frame tag for the connect "hello": the **device** (Android app) sends one hello frame the
/// instant it opens the accessory, and the **host** reads it before writing anything. This
/// avoids a startup-ordering deadlock observed on real hardware — the host's first bulk-OUT
/// write can otherwise land before the app has opened `/dev/usb_accessory`, and the gadget
/// silently drops it (device→host bulk IN buffers reliably; host→device bulk OUT does not if no
/// reader is attached). The payload is empty; the host discards the tag (so the value only has
/// to be agreed, not interpreted) — kept here as the single source of truth for both ends.
pub const HELLO_TAG: u8 = 2;

pub fn protocol_version() -> u32 {
    1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_is_one() {
        assert_eq!(protocol_version(), 1);
    }
}
