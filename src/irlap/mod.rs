//! IrLAP — Infrared Link Access Protocol (subset).
//!
//! M5 implements **discovery** only: build XID discovery command frames, send
//! them, and parse XID responses. Frame layout and constants are taken verbatim
//! from the Linux stack (`net/irda/irlap_frame.c`, `include/net/irda/*.h`) — see
//! ANALYSIS.md §6. Nothing here is guessed.
//!
//! An IrLAP frame is carried inside a SIR async wrapper (see `sir::framing`);
//! this module deals only with the unwrapped frame body.

use std::time::{Duration, Instant};

use crate::chip::Stir;
use crate::sir::framing::Unwrapper;

// Address / control constants (`irlap.h`, `irlap_frame.h`).
const CBROADCAST: u8 = 0xfe; // connection broadcast address
const CMD_FRAME: u8 = 0x01; // command/response bit (set = command)
const XID_CMD: u8 = 0x2f; // XID command control
const XID_RSP: u8 = 0xaf; // XID response control
const PF_BIT: u8 = 0x10; // poll/final bit
const XID_FORMAT: u8 = 0x01; // discovery XID format id
const BROADCAST: u32 = 0xffff_ffff; // broadcast device address
const HINT_EXTENSION: u8 = 0x80; // "more hint bytes follow"
const SLOT_FINAL: u8 = 0xff; // final-slot marker

/// The fixed part of an XID frame is 14 bytes (`struct xid_frame`).
const XID_FIXED_LEN: usize = 14;

/// Our own station identity, advertised in the final discovery slot.
pub struct SelfInfo {
    pub saddr: u32,
    pub hints: [u8; 2],
    pub charset: u8,
    pub name: String,
}

impl Default for SelfInfo {
    fn default() -> Self {
        // Present ourselves as a computer, ASCII charset. hints[0] has no
        // EXTENSION bit, so only one hint byte is sent.
        Self {
            saddr: 0x0000_cafe,
            hints: [0x04 /* HINT_COMPUTER */, 0x00],
            charset: 0x00, // CS_ASCII
            name: "mac".to_string(),
        }
    }
}

/// A device discovered via XID response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveredDevice {
    /// Device address to use when connecting (the responder's `saddr`).
    pub address: u32,
    pub hints: [u8; 2],
    pub charset: u8,
    pub name: String,
}

/// Encode the slot count `S` into the XID `flags` field
/// (`irlap_frame.c:353-370`).
fn slot_flags(s: u8) -> u8 {
    match s {
        1 => 0x00,
        6 => 0x01,
        8 => 0x02,
        16 => 0x03,
        _ => 0x01,
    }
}

/// Build an XID discovery **command** frame body (no SIR wrapping / FCS).
/// `info` is appended only for the final slot.
pub fn build_xid_command(saddr: u32, s: u8, slot: u8, info: Option<&SelfInfo>) -> Vec<u8> {
    let mut f = Vec::with_capacity(XID_FIXED_LEN + 8);
    f.push(CBROADCAST | CMD_FRAME); // caddr = 0xff
    f.push(XID_CMD | PF_BIT); // control = 0x3f
    f.push(XID_FORMAT); // 0x01
    f.extend_from_slice(&saddr.to_le_bytes()); // saddr (LE)
    f.extend_from_slice(&BROADCAST.to_le_bytes()); // daddr = broadcast
    f.push(slot_flags(s)); // flags
    f.push(slot); // slotnr
    f.push(0x00); // version

    if slot == SLOT_FINAL {
        if let Some(info) = info {
            if info.hints[0] & HINT_EXTENSION != 0 {
                f.push(info.hints[0]);
                f.push(info.hints[1]);
            } else {
                f.push(info.hints[0]);
            }
            f.push(info.charset);
            f.extend_from_slice(info.name.as_bytes());
        }
    }
    f
}

/// Parse an unwrapped IrLAP frame body as an XID discovery **response**.
/// Returns `None` if it is not a well-formed XID response.
pub fn parse_xid_response(body: &[u8]) -> Option<DiscoveredDevice> {
    if body.len() < XID_FIXED_LEN {
        return None;
    }
    // control may or may not carry the P/F bit.
    if body[1] & !PF_BIT != XID_RSP {
        return None;
    }
    if body[2] != XID_FORMAT {
        return None;
    }

    // The responder's source address is the device address we connect to
    // (`irlap_frame.c:426`: info->daddr = xid->saddr).
    let address = u32::from_le_bytes([body[3], body[4], body[5], body[6]]);

    // Discovery info follows the 14-byte fixed part (`irlap_frame.c:448-467`).
    let info = &body[XID_FIXED_LEN..];
    let (hints, charset_idx) = if !info.is_empty() && info[0] & HINT_EXTENSION != 0 {
        (
            [info[0], info.get(1).copied().unwrap_or(0)],
            2usize,
        )
    } else {
        ([info.first().copied().unwrap_or(0), 0], 1usize)
    };
    let charset = info.get(charset_idx).copied().unwrap_or(0);
    let name_bytes = info.get(charset_idx + 1..).unwrap_or(&[]);
    let name = String::from_utf8_lossy(name_bytes)
        .trim_end_matches('\0')
        .to_string();

    Some(DiscoveredDevice {
        address,
        hints,
        charset,
        name,
    })
}

/// Read the bulk IN endpoint for `window`, de-wrapping any SIR frames and
/// collecting XID responses into `found` (de-duplicated by address).
fn collect_responses(
    stir: &Stir,
    unwrapper: &mut Unwrapper,
    window: Duration,
    found: &mut Vec<DiscoveredDevice>,
) {
    let mut buf = vec![0u8; 4096];
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match stir.read_bulk_in(&mut buf, Duration::from_millis(20)) {
            Ok(n) if n > 0 => {
                crate::logging::dump_frame("RX", &buf[..n]);
                for body in unwrapper.push_all(&buf[..n]) {
                    if let Some(dev) = parse_xid_response(&body) {
                        if !found.iter().any(|d| d.address == dev.address) {
                            found.push(dev);
                        }
                    }
                }
            }
            Ok(_) => {}
            Err(rusb::Error::Timeout) => {}
            Err(e) => crate::error!("bulk IN read error during discovery: {e}"),
        }
    }
}

/// Run one discovery pass with `s` slots, listening `slot_window` after each
/// slot command (and the final info command). Returns the devices found.
pub fn discover(
    stir: &Stir,
    me: &SelfInfo,
    s: u8,
    slot_window: Duration,
) -> Result<Vec<DiscoveredDevice>, Box<dyn std::error::Error>> {
    let mut found = Vec::new();
    let mut unwrapper = Unwrapper::new();

    for slot in 0..s {
        let frame = build_xid_command(me.saddr, s, slot, None);
        stir.send_sir(&frame)?;
        let _ = stir.fifo_drain(Duration::from_millis(60));
        collect_responses(stir, &mut unwrapper, slot_window, &mut found);
    }

    // Final slot carries our own identity and ends the discovery.
    let frame = build_xid_command(me.saddr, s, SLOT_FINAL, Some(me));
    stir.send_sir(&frame)?;
    let _ = stir.fifo_drain(Duration::from_millis(60));
    collect_responses(stir, &mut unwrapper, slot_window, &mut found);

    Ok(found)
}

/// M5 entry point: initialise the chip, run IrLAP discovery, and print any
/// devices found (address + nickname). Acceptance: the Galileo responds.
pub fn run_discovery(
    vid: u16,
    pid: u16,
    speed: u32,
    slots: u8,
    retries: u32,
) -> Result<(), Box<dyn std::error::Error>> {
    let (handle, iface) = crate::usb::open_claimed(vid, pid)?;
    println!("Claimed interface {iface}. IrLAP discovery at {speed} baud, S={slots} slots...");

    let _ = handle.clear_halt(crate::chip::EP_BULK_OUT);
    let _ = handle.clear_halt(crate::chip::EP_BULK_IN);

    let stir = Stir::new(&handle);
    stir.change_speed(speed)?;

    let me = SelfInfo::default();
    let slot_window = Duration::from_millis(200);

    for attempt in 1..=retries {
        println!("\nDiscovery attempt {attempt}/{retries}...");
        let found = discover(&stir, &me, slots, slot_window)?;
        if !found.is_empty() {
            println!("\nM5 OK: {} device(s) discovered:", found.len());
            for d in &found {
                println!(
                    "  address=0x{:08x}  hints={:02x}{:02x}  charset=0x{:02x}  name=\"{}\"",
                    d.address, d.hints[0], d.hints[1], d.charset, d.name
                );
            }
            return Ok(());
        }
    }

    println!(
        "\nNo device responded to discovery. Ensure the Galileo is on and its IR window faces \
         the dongle. Timing may need tuning (see brief §6 / NOTES.md); try --slots 6 or -v."
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_frame_header_is_correct() {
        let f = build_xid_command(0x0000cafe, 1, 0, None);
        assert_eq!(f.len(), XID_FIXED_LEN);
        assert_eq!(f[0], 0xff); // caddr = CBROADCAST | CMD_FRAME
        assert_eq!(f[1], 0x3f); // control = XID_CMD | PF_BIT
        assert_eq!(f[2], 0x01); // XID_FORMAT
        assert_eq!(&f[3..7], &0x0000cafeu32.to_le_bytes()); // saddr
        assert_eq!(&f[7..11], &0xffffffffu32.to_le_bytes()); // daddr broadcast
        assert_eq!(f[11], 0x00); // flags for S=1
        assert_eq!(f[12], 0x00); // slot 0
        assert_eq!(f[13], 0x00); // version
    }

    #[test]
    fn final_slot_appends_info() {
        let me = SelfInfo {
            saddr: 0x11223344,
            hints: [0x04, 0x00],
            charset: 0x00,
            name: "mac".to_string(),
        };
        let f = build_xid_command(me.saddr, 1, SLOT_FINAL, Some(&me));
        assert_eq!(f[12], 0xff); // final slot
        assert_eq!(f[14], 0x04); // hint byte (no extension)
        assert_eq!(f[15], 0x00); // charset
        assert_eq!(&f[16..], b"mac"); // nickname
    }

    #[test]
    fn parse_roundtrip_response() {
        // Craft an XID response for a device at 0xdeadbeef named "GALILEO".
        let mut r = vec![
            CBROADCAST,       // caddr
            XID_RSP | PF_BIT, // control 0xbf
            XID_FORMAT,       // 0x01
        ];
        r.extend_from_slice(&0xdeadbeefu32.to_le_bytes()); // saddr (device)
        r.extend_from_slice(&0x0000cafeu32.to_le_bytes()); // daddr (us)
        r.push(0x00); // flags
        r.push(0x00); // slot
        r.push(0x00); // version
        r.push(0x04); // hint (computer, no extension)
        r.push(0x00); // charset ASCII
        r.extend_from_slice(b"GALILEO");

        let dev = parse_xid_response(&r).expect("should parse");
        assert_eq!(dev.address, 0xdeadbeef);
        assert_eq!(dev.hints, [0x04, 0x00]);
        assert_eq!(dev.name, "GALILEO");
    }

    #[test]
    fn rejects_non_xid_response() {
        let mut r = vec![0xfe, 0x11, 0x01]; // control not XID_RSP
        r.extend_from_slice(&[0u8; 11]);
        assert!(parse_xid_response(&r).is_none());
    }

    #[test]
    fn parse_response_with_hint_extension() {
        let mut r = vec![CBROADCAST, XID_RSP, XID_FORMAT];
        r.extend_from_slice(&0x01020304u32.to_le_bytes());
        r.extend_from_slice(&0u32.to_le_bytes());
        r.extend_from_slice(&[0x00, 0x00, 0x00]); // flags/slot/version
        r.push(0x80 | 0x04); // hint[0] with EXTENSION
        r.push(0x02); // hint[1]
        r.push(0x00); // charset
        r.extend_from_slice(b"Dev");
        let dev = parse_xid_response(&r).unwrap();
        assert_eq!(dev.hints, [0x84, 0x02]);
        assert_eq!(dev.name, "Dev");
    }
}
