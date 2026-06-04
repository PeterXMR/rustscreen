//! Android Open Accessory (AOA) host adapter — the live USB path (RESEARCH Patterns 1+2).
//! Compiled ONLY under `--features live-usb` so default Wave A CI stays clean and
//! cross-platform (no `nusb`, no macOS-only code).
//!
//! Flow: enumerate → AOA handshake (control req 51 get-protocol, req 52 the six identity
//! strings, req 53 start) → the device re-enumerates as VID `0x18D1` / PID `0x2D00..=0x2D05`
//! → open a NEW handle (never reuse the pre-handshake one — Pitfall 2) → claim interface 0 →
//! read the bulk IN/OUT endpoint addresses from the descriptor → drive [`AoaTransport`]
//! (which is `Read + Write + Send`, so it satisfies [`crate::transport::Transport`]).
//!
//! The identity strings ([`IDENTITY`]) are defined ONCE here and MUST match
//! `android/app/src/main/res/xml/accessory_filter.xml` verbatim (RESEARCH Pitfall 4): the
//! manufacturer/model/version the filter matches are strings 0/1/3 below.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use nusb::transfer::{Bulk, ControlIn, ControlOut, ControlType, Direction, In, Out, Recipient};
use nusb::{Device, DeviceInfo, Interface, MaybeFuture};

/// AOA control requests (vendor, device-recipient) — AOSP AOA 1.0 spec.
pub const AOA_GET_PROTOCOL: u8 = 51;
pub const AOA_SEND_STRING: u8 = 52;
pub const AOA_START: u8 = 53;

/// Google's accessory-mode vendor id, and the PID range a device re-enumerates into after
/// AOA START (0x2D00 = accessory, 0x2D01 = accessory+ADB, 0x2D02..=0x2D05 add audio/HID).
const AOA_VID: u16 = 0x18D1;
const AOA_PID_LO: u16 = 0x2D00;
const AOA_PID_HI: u16 = 0x2D05;

/// Per-transfer chunk / control timeout window.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(1);
/// Bulk transfer size — chunked at the AOA accessory buffer (Pitfall 3). Sourced from the
/// shared [`protocol::BULK_TRANSFER_SIZE`] so the host's per-transfer size and the device's
/// accessory read size can never drift apart (see that constant for the hardware rationale).
const BULK_CHUNK: usize = protocol::BULK_TRANSFER_SIZE;

/// The six AOA identity strings sent in control request 52, by string id (0..=5).
///
/// **MUST stay verbatim-equal to `res/xml/accessory_filter.xml`** — Android matches the
/// manufacturer (0), model (1) and version (3) against the filter; a mismatch means the
/// phone never fires the `USB_ACCESSORY_ATTACHED` intent (RESEARCH Pitfall 4).
pub const IDENTITY: [(u16, &str); 6] = [
    (0, "RustScreen"),                             // manufacturer
    (1, "RustScreen Host"),                        // model
    (2, "USB second-monitor link"),                // description
    (3, "1.0"),                                    // version
    (4, "https://github.com/PeterXMR/rustscreen"), // uri
    (5, "rs-0001"),                                // serial
];

/// Map a `nusb` transfer error into an `io::Error` so the whole path speaks `io::Result`
/// (and rides the `Transport` seam unchanged).
fn io_err<E: std::fmt::Display>(ctx: &str, e: E) -> io::Error {
    io::Error::other(format!("{ctx}: {e}"))
}

/// Drive the AOA handshake via **device-level control transfers** on the *pre-accessory*
/// device — deliberately WITHOUT claiming an interface. AOA requests are addressed to the
/// device's default control endpoint (recipient = Device), so claiming interface 0 is
/// unnecessary, and on macOS it is actively harmful: the OS binds a class driver to the
/// phone's interface 0 and rejects `claim_interface` with `kIOReturnExclusiveAccess`
/// (0xe00002c5). `Device::control_*` sidesteps that entirely (no `sudo` needed). req 51
/// (assert protocol >= 1), req 52 for each identity string (exact UTF-8 bytes, no trailing
/// NUL — HI-02), req 53 to start. After this the device re-enumerates — call [`reacquire`],
/// then claim the *accessory* interface (vendor-specific, no macOS driver bound).
pub fn handshake(dev: &Device) -> io::Result<u16> {
    let ver = dev
        .control_in(
            ControlIn {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: AOA_GET_PROTOCOL,
                value: 0,
                index: 0,
                length: 2,
            },
            CONTROL_TIMEOUT,
        )
        .wait()
        .map_err(|e| io_err("AOA get-protocol (req 51)", e))?;
    if ver.len() < 2 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "AOA get-protocol returned < 2 bytes",
        ));
    }
    let proto = u16::from_le_bytes([ver[0], ver[1]]);
    if proto < 1 {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("device reports AOA protocol {proto}, need >= 1"),
        ));
    }
    eprintln!("  req 51 get-protocol → AOA version {proto}");

    for (idx, s) in IDENTITY {
        // HI-02: send the EXACT UTF-8 bytes with wLength = bytes.len() and NO manually
        // appended NUL. IDENTITY (above) is the single source of truth and must match
        // `accessory_filter.xml` char-for-char; appending `\0` would make the on-wire string
        // one byte longer than the XML and risk the filter never matching.
        //
        // NOTE: if the live Task B3 test shows NO permission dialog appearing, the device may
        // expect the alternate AOSP convention of a NUL-terminated string — in that case send
        // `len + 1` with a single trailing 0 byte instead (try both before concluding the
        // strings are wrong).
        let data = s.as_bytes();
        dev.control_out(
            ControlOut {
                control_type: ControlType::Vendor,
                recipient: Recipient::Device,
                request: AOA_SEND_STRING,
                value: 0,
                index: idx,
                data,
            },
            CONTROL_TIMEOUT,
        )
        .wait()
        .map_err(|e| io_err("AOA send-string (req 52)", e))?;
    }
    eprintln!("  req 52 identity strings sent");

    dev.control_out(
        ControlOut {
            control_type: ControlType::Vendor,
            recipient: Recipient::Device,
            request: AOA_START,
            value: 0,
            index: 0,
            data: &[],
        },
        CONTROL_TIMEOUT,
    )
    .wait()
    .map_err(|e| io_err("AOA start (req 53)", e))?;
    eprintln!("  req 53 start sent — device should now re-enumerate in accessory mode");
    Ok(proto)
}

/// Find a candidate device to drive the handshake.
///
/// ME-01: do NOT grab "the first device with a non-zero vendor id" — that picks hubs,
/// keyboards or whatever enumerates first. The Pixel presents as Google VID `0x18D1` in
/// normal (pre-accessory) mode, so prefer that; a device already in the AOA PID range is
/// also a valid target (e.g. a re-run after a partial handshake). If neither is present,
/// return an actionable error rather than handshaking a random peripheral.
pub fn find_candidate() -> io::Result<DeviceInfo> {
    let devices: Vec<DeviceInfo> = nusb::list_devices()
        .wait()
        .map_err(|e| io_err("list_devices", e))?
        .collect();

    // A Google-VID device in normal mode, OR any device already in the AOA accessory range.
    devices
        .iter()
        .find(|d| d.vendor_id() == AOA_VID && (AOA_PID_LO..=AOA_PID_HI).contains(&d.product_id()))
        .or_else(|| devices.iter().find(|d| d.vendor_id() == AOA_VID))
        .cloned()
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!(
                    "no Google/Pixel USB device found (looked for VID {AOA_VID:#06x} or AOA PID \
                     {AOA_PID_LO:#06x}..={AOA_PID_HI:#06x}). Connect the Pixel via USB-C, unlock \
                     it, and ensure it is in file-transfer/MTP (not charge-only) mode."
                ),
            )
        })
}

/// Poll `list_devices()` until the accessory-mode device (VID 0x18D1 / PID 0x2D00..=0x2D05)
/// appears, then open a NEW handle (never reuse the pre-handshake one — Pitfall 2).
pub fn reacquire(timeout: Duration) -> io::Result<Device> {
    let deadline = Instant::now() + timeout;
    let mut tick = 0u32;
    loop {
        let devices: Vec<DeviceInfo> = nusb::list_devices()
            .wait()
            .map_err(|e| io_err("list_devices (reacquire)", e))?
            .collect();

        if let Some(info) = devices.iter().find(|d| {
            d.vendor_id() == AOA_VID && (AOA_PID_LO..=AOA_PID_HI).contains(&d.product_id())
        }) {
            eprintln!(
                "  accessory re-enumerated as {:04x}:{:04x} ✓",
                info.vendor_id(),
                info.product_id()
            );
            return info.open().wait().map_err(|e| io_err("open accessory", e));
        }

        // Once per ~second, print what IS on the bus so we can see what the phone became
        // (e.g. reverted to its normal Google PID, or dropped off entirely).
        // (`% 10 == 0`, not `is_multiple_of`, to stay within the project's 1.80 MSRV.)
        if tick % 10 == 0 {
            let google: Vec<String> = devices
                .iter()
                .filter(|d| d.vendor_id() == AOA_VID)
                .map(|d| format!("{:04x}:{:04x}", d.vendor_id(), d.product_id()))
                .collect();
            eprintln!(
                "  …waiting for accessory PID {AOA_PID_LO:#06x}..={AOA_PID_HI:#06x}; on bus now: {} total dev, Google: [{}]",
                devices.len(),
                if google.is_empty() { "none".into() } else { google.join(", ") }
            );
        }
        tick += 1;

        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "accessory-mode device did not re-enumerate within timeout (see the bus snapshots above — if the phone reverted to its normal PID it rejected accessory mode; if it vanished it may need a replug)",
            ));
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Claim interface 0 of an accessory-mode device and build the bulk IN/OUT transport by
/// reading the endpoint addresses from the interface descriptor (A6 — not hard-coded).
pub fn open_transport(dev: &Device) -> io::Result<AoaTransport> {
    let iface = dev.claim_interface(0).wait().map_err(|e| {
        // ME-02: a bare nusb error here is opaque. On macOS, claiming a USB interface
        // typically fails because another process (or the kernel) already owns it, or
        // because we lack privileges — surface the likely fix.
        io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!(
                "claim_interface(0) failed ({e}). On macOS the interface may be in use by \
                 another process or require elevated privileges — try re-running with `sudo`, \
                 and ensure no other app (e.g. Android File Transfer) holds the device."
            ),
        )
    })?;

    let desc = iface
        .descriptor()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no active interface descriptor"))?;

    let mut bulk_in: Option<u8> = None;
    let mut bulk_out: Option<u8> = None;
    for ep in desc.endpoints() {
        if ep.transfer_type() == nusb::descriptors::TransferType::Bulk {
            match ep.direction() {
                Direction::In => bulk_in = Some(ep.address()),
                Direction::Out => bulk_out = Some(ep.address()),
            }
        }
    }
    let in_addr =
        bulk_in.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no bulk IN endpoint"))?;
    let out_addr =
        bulk_out.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no bulk OUT endpoint"))?;

    let ep_out = iface
        .endpoint::<Bulk, Out>(out_addr)
        .map_err(|e| io_err("open bulk OUT endpoint", e))?;
    let ep_in = iface
        .endpoint::<Bulk, In>(in_addr)
        .map_err(|e| io_err("open bulk IN endpoint", e))?;

    Ok(AoaTransport {
        writer: nusb::io::EndpointWrite::new(ep_out, BULK_CHUNK),
        reader: nusb::io::EndpointRead::new(ep_in, BULK_CHUNK),
        _iface: iface,
    })
}

/// A `Read + Write + Send` transport over an accessory-mode device's bulk endpoints —
/// satisfies [`crate::transport::Transport`], so the spike drives it via `echo_roundtrip`.
/// Writes chunk at the 16 KiB AOA buffer; `EndpointRead` reconciles partial reads.
pub struct AoaTransport {
    writer: nusb::io::EndpointWrite<Bulk>,
    reader: nusb::io::EndpointRead<Bulk>,
    // Held to keep the claimed interface alive for the endpoints' lifetime.
    _iface: Interface,
}

impl Read for AoaTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.reader.read(buf)
    }
}

impl Write for AoaTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.writer.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

/// NCM/TCP fallback transport (RESEARCH Pattern 5). A `TcpStream` is already
/// `Read + Write + Send`; this newtype exists only to name the D1 fallback path. The same
/// `echo_roundtrip` drives it unchanged.
pub struct NcmTransport(pub TcpStream);

impl Read for NcmTransport {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.0.read(buf)
    }
}

impl Write for NcmTransport {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl NcmTransport {
    /// Connect a TCP stream to the phone's NCM-tethered listener.
    pub fn connect(addr: &str) -> io::Result<Self> {
        Ok(NcmTransport(TcpStream::connect(addr)?))
    }
}
