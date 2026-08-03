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

// Connection-management control fields (`irlap_frame.h`).
const SNRM_CMD: u8 = 0x83;
const UA_RSP: u8 = 0x63;
const DISC_CMD: u8 = 0x43;
const RR: u8 = 0x01;

// QoS negotiation parameter identifiers (`qos.h`), in SNRM insertion order.
const PI_BAUD_RATE: u8 = 0x01;
const PI_MAX_TURN_TIME: u8 = 0x82;
const PI_DATA_SIZE: u8 = 0x83;
const PI_WINDOW_SIZE: u8 = 0x84;
const PI_ADD_BOFS: u8 = 0x85;
const PI_MIN_TURN_TIME: u8 = 0x86;
const PI_LINK_DISC: u8 = 0x08;

// QoS value tables (`qos.c:103-109`), for decoding the negotiated UA.
const BAUD_RATES: [u32; 10] = [
    2400, 9600, 19200, 38400, 57600, 115200, 576000, 1152000, 4000000, 16000000,
];
const DATA_SIZES: [u32; 6] = [64, 128, 256, 512, 1024, 2048];
const ADD_BOFS: [u32; 8] = [48, 24, 12, 5, 3, 2, 1, 0];
const MAX_TURN_TIMES: [u32; 4] = [500, 250, 100, 50];
const MIN_TURN_TIMES: [u32; 8] = [10000, 5000, 1000, 500, 100, 50, 10, 0];
const LINK_DISC_TIMES: [u32; 8] = [3, 8, 12, 16, 20, 25, 30, 40];

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

// ------------------------------------------------------------------------
// M6 — IrLAP connection (SNRM/UA handshake + NRM keepalive)
// ------------------------------------------------------------------------

/// The QoS parameters we advertise in the SNRM. Each entry is `(PI, PV)` with a
/// 1-byte value, in the exact order the Linux stack inserts them
/// (`qos.c:463-...`). PV is a bitmask over the value tables. We deliberately:
/// - offer **9600 only** (bit 1) so the link never changes speed mid-session;
/// - offer **500 ms max turnaround** (bit 0) to give our userspace a generous
///   turnaround budget;
/// - offer **window 1** and **data size 64** (simplest, mandatory values);
/// - offer **any** additional-BOFs / min-turn / link-disconnect so the
///   negotiation always intersects with whatever the device supports.
const SNRM_QOS_PARAMS: [(u8, u8); 7] = [
    (PI_BAUD_RATE, 0x02),     // 9600
    (PI_MAX_TURN_TIME, 0x01), // 500 ms
    (PI_DATA_SIZE, 0x01),     // 64 bytes
    (PI_WINDOW_SIZE, 0x01),   // window 1
    (PI_ADD_BOFS, 0xff),      // any
    (PI_MIN_TURN_TIME, 0xff), // any
    (PI_LINK_DISC, 0xff),     // any
];

/// Negotiated QoS decoded from a UA (best-effort, for logging).
#[derive(Debug, Default, Clone)]
pub struct NegotiatedQos {
    pub baud: Option<u32>,
    pub max_turn_ms: Option<u32>,
    pub data_size: Option<u32>,
    pub window: Option<u32>,
    pub add_bofs: Option<u32>,
    pub min_turn_us: Option<u32>,
    pub link_disc_s: Option<u32>,
}

/// A very weak PRNG seeded from the clock — enough for choosing session-local
/// IrDA addresses without pulling in a dependency.
fn weak_rand() -> u32 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let n = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut x = (n as u64) ^ 0x9e37_79b9_7f4a_7c15;
    x ^= x >> 30;
    x = x.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    x ^= x >> 27;
    x = x.wrapping_mul(0x94d0_49bb_1331_11eb);
    x ^= x >> 31;
    x as u32
}

/// Public wrapper so other layers can seed a session-local source address.
pub fn weak_rand_pub() -> u32 {
    weak_rand()
}

/// Pick a random connection address: even (bit 0 clear), not 0x00 or 0xfe
/// (`irlap.c:933-938`).
fn random_caddr() -> u8 {
    loop {
        let c = (weak_rand() as u8) & 0xfe;
        if c != 0x00 && c != 0xfe {
            return c;
        }
    }
}

/// Build an SNRM connect command: fixed 11-byte header + QoS parameters.
pub fn build_snrm(saddr: u32, daddr: u32, ncaddr: u8) -> Vec<u8> {
    let mut f = Vec::with_capacity(11 + SNRM_QOS_PARAMS.len() * 3);
    f.push(CBROADCAST | CMD_FRAME); // caddr = 0xff
    f.push(SNRM_CMD | PF_BIT); // control = 0x93
    f.extend_from_slice(&saddr.to_le_bytes());
    f.extend_from_slice(&daddr.to_le_bytes());
    f.push(ncaddr); // new connection address
    for (pi, pv) in SNRM_QOS_PARAMS {
        f.push(pi);
        f.push(0x01); // PL = 1
        f.push(pv);
    }
    f
}

/// Build an RR (Receive Ready) supervisory frame (`irlap_frame.c`
/// `irlap_send_rr_frame`): `control = RR | PF | (vr << 5)`.
pub fn build_rr(caddr: u8, vr: u8, command: bool) -> Vec<u8> {
    let addr = if command { caddr | CMD_FRAME } else { caddr };
    vec![addr, RR | PF_BIT | (vr << 5)]
}

/// Build a DISC command frame.
pub fn build_disc(caddr: u8) -> Vec<u8> {
    vec![caddr | CMD_FRAME, DISC_CMD | PF_BIT]
}

fn decode_pv(pv: u8, table: &[u32]) -> Option<u32> {
    // The UA usually returns a single chosen bit; take the lowest set bit.
    if pv == 0 {
        return None;
    }
    let idx = pv.trailing_zeros() as usize;
    table.get(idx).copied()
}

/// Parse a UA response body: verify it, extract addresses, and decode the
/// negotiated QoS parameters. `conn` is the connection address we proposed.
pub fn parse_ua(body: &[u8]) -> Option<(u32, u32, NegotiatedQos)> {
    if body.len() < 10 {
        return None;
    }
    if body[1] & !PF_BIT != UA_RSP {
        return None;
    }
    let saddr = u32::from_le_bytes([body[2], body[3], body[4], body[5]]);
    let daddr = u32::from_le_bytes([body[6], body[7], body[8], body[9]]);

    let mut qos = NegotiatedQos::default();
    let mut i = 10;
    while i + 2 <= body.len() {
        let pi = body[i];
        let pl = body[i + 1] as usize;
        if i + 2 + pl > body.len() || pl == 0 {
            break;
        }
        let pv = body[i + 2]; // all our params are 1-byte
        match pi {
            PI_BAUD_RATE => qos.baud = decode_pv(pv, &BAUD_RATES),
            PI_MAX_TURN_TIME => qos.max_turn_ms = decode_pv(pv, &MAX_TURN_TIMES),
            PI_DATA_SIZE => qos.data_size = decode_pv(pv, &DATA_SIZES),
            PI_WINDOW_SIZE => qos.window = decode_pv(pv, &[1, 2, 3, 4, 5, 6, 7]),
            PI_ADD_BOFS => qos.add_bofs = decode_pv(pv, &ADD_BOFS),
            PI_MIN_TURN_TIME => qos.min_turn_us = decode_pv(pv, &MIN_TURN_TIMES),
            PI_LINK_DISC => qos.link_disc_s = decode_pv(pv, &LINK_DISC_TIMES),
            _ => {}
        }
        i += 2 + pl;
    }
    Some((saddr, daddr, qos))
}

/// Send `frame`, drain the TX FIFO, then listen up to `window` for a de-wrapped
/// response whose connection address matches `conn` (either direction bit).
/// Returns the first matching frame body.
fn exchange(
    stir: &Stir,
    unwrapper: &mut Unwrapper,
    frame: &[u8],
    conn_match: impl Fn(&[u8]) -> bool,
    window: Duration,
) -> Result<Option<Vec<u8>>, Box<dyn std::error::Error>> {
    stir.send_sir(frame)?;
    let _ = stir.fifo_drain(Duration::from_millis(60));

    let mut buf = vec![0u8; 4096];
    let deadline = Instant::now() + window;
    while Instant::now() < deadline {
        match stir.read_bulk_in(&mut buf, Duration::from_millis(20)) {
            Ok(n) if n > 0 => {
                crate::logging::dump_frame("RX", &buf[..n]);
                for body in unwrapper.push_all(&buf[..n]) {
                    if conn_match(&body) {
                        return Ok(Some(body));
                    }
                }
            }
            Ok(_) => {}
            Err(rusb::Error::Timeout) => {}
            Err(e) => crate::error!("bulk IN read error: {e}"),
        }
    }
    Ok(None)
}

/// An established IrLAP connection where we act as the NRM **primary**.
/// Tracks the send/receive sequence numbers and drives the poll/final exchange.
/// Window size is 1 (as negotiated), so each poll yields exactly one secondary
/// frame ending with the F bit.
pub struct IrlapLink {
    unwrapper: Unwrapper,
    conn: u8,
    vs: u8, // V(s), our send sequence number
    vr: u8, // V(r), our receive sequence number
    resp_window: Duration,
    retries: u32,
}

impl IrlapLink {
    pub fn conn(&self) -> u8 {
        self.conn
    }

    /// One primary transaction: send a frame with the poll bit (an I-frame if
    /// `info` is given, otherwise RR), then read the secondary's response until
    /// its final bit or timeout. Updates V(s)/V(r). Returns
    /// `(responded, info)` where `responded` is true if any matching frame came
    /// back, and `info` is the secondary's I-field if it sent one.
    pub fn transact(
        &mut self,
        stir: &Stir,
        info: Option<&[u8]>,
    ) -> Result<(bool, Option<Vec<u8>>), Box<dyn std::error::Error>> {
        let sent_i = info.is_some();
        let frame = match info {
            Some(data) => {
                // I-frame: control = N(r)<<5 | P | N(s)<<1 (bit0 = 0).
                let control = (self.vr << 5) | PF_BIT | (self.vs << 1);
                let mut f = Vec::with_capacity(2 + data.len());
                f.push(self.conn | CMD_FRAME);
                f.push(control);
                f.extend_from_slice(data);
                f
            }
            None => build_rr(self.conn, self.vr, true),
        };

        for _try in 0..self.retries {
            stir.send_sir(&frame)?;
            let _ = stir.fifo_drain(Duration::from_millis(60));

            let mut buf = vec![0u8; 4096];
            let deadline = Instant::now() + self.resp_window;
            while Instant::now() < deadline {
                let n = match stir.read_bulk_in(&mut buf, Duration::from_millis(20)) {
                    Ok(n) if n > 0 => n,
                    Ok(_) => continue,
                    Err(rusb::Error::Timeout) => continue,
                    Err(e) => {
                        crate::error!("bulk IN read error: {e}");
                        continue;
                    }
                };
                crate::logging::dump_frame("RX", &buf[..n]);
                for body in self.unwrapper.push_all(&buf[..n]) {
                    if body.len() < 2 || (body[0] & 0xfe) != self.conn {
                        continue;
                    }
                    let control = body[1];
                    if control & 0x01 == 0 {
                        // I-frame from the secondary.
                        let ns = (control >> 1) & 0x07;
                        let nr = (control >> 5) & 0x07;
                        let pf = (control >> 4) & 1;
                        if sent_i && nr == ((self.vs + 1) & 0x07) {
                            self.vs = (self.vs + 1) & 0x07;
                        }
                        let mut out = None;
                        if ns == self.vr {
                            self.vr = (self.vr + 1) & 0x07;
                            out = Some(body[2..].to_vec());
                        }
                        if pf == 1 {
                            return Ok((true, out));
                        }
                    } else {
                        // Supervisory frame (RR/RNR/REJ).
                        let nr = (control >> 5) & 0x07;
                        let pf = (control >> 4) & 1;
                        if sent_i && nr == ((self.vs + 1) & 0x07) {
                            self.vs = (self.vs + 1) & 0x07;
                        }
                        if pf == 1 {
                            return Ok((true, None));
                        }
                    }
                }
            }
            // No final-bit response within the window: retransmit (V(s) not
            // advanced yet, so this is a legitimate NRM retransmission).
        }
        Ok((false, None))
    }

    /// Send a DISC command (best-effort clean disconnect).
    pub fn disconnect(&mut self, stir: &Stir) {
        let _ = stir.send_sir(&build_disc(self.conn));
        let _ = stir.fifo_drain(Duration::from_millis(60));
    }
}

/// Discover the device and bring up an IrLAP connection (SNRM/UA). Returns the
/// established link, the discovered device, and the negotiated QoS.
pub fn connect(
    stir: &Stir,
    saddr: u32,
) -> Result<(IrlapLink, DiscoveredDevice, NegotiatedQos), Box<dyn std::error::Error>> {
    let me = SelfInfo {
        saddr,
        ..SelfInfo::default()
    };

    // 1. Discover the device to obtain its (per-session) address.
    let mut device = None;
    for _ in 0..4 {
        if let Some(d) = discover(stir, &me, 1, Duration::from_millis(200))?
            .into_iter()
            .next()
        {
            device = Some(d);
            break;
        }
    }
    let device = device.ok_or("no device responded to discovery")?;

    // 2. SNRM/UA handshake (retried).
    let conn = random_caddr();
    let snrm = build_snrm(saddr, device.address, conn);
    let ua_match = |b: &[u8]| b.len() >= 2 && (b[0] & 0xfe) == conn && (b[1] & !PF_BIT) == UA_RSP;

    let mut unwrapper = Unwrapper::new();
    let mut established = None;
    for _ in 0..6 {
        if let Some(body) = exchange(
            stir,
            &mut unwrapper,
            &snrm,
            ua_match,
            Duration::from_millis(500),
        )? {
            established = parse_ua(&body);
            if established.is_some() {
                break;
            }
        }
    }
    let (_sa, _da, qos) = established.ok_or("no UA received — connection failed")?;

    let link = IrlapLink {
        unwrapper,
        conn,
        vs: 0,
        vr: 0,
        resp_window: Duration::from_millis(500),
        retries: 4,
    };
    Ok((link, device, qos))
}

/// M6 entry point: discover, establish an IrLAP connection (SNRM/UA), and hold
/// it with RR keepalives for `hold_secs`.
pub fn run_connect(
    vid: u16,
    pid: u16,
    speed: u32,
    hold_secs: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    let (handle, iface) = crate::usb::open_claimed(vid, pid)?;
    println!("Claimed interface {iface}. IrLAP connect at {speed} baud...");

    let _ = handle.clear_halt(crate::chip::EP_BULK_OUT);
    let _ = handle.clear_halt(crate::chip::EP_BULK_IN);

    let stir = Stir::new(&handle);
    stir.change_speed(speed)?;

    let saddr = weak_rand() | 0x0000_0001;
    println!("Discovering and connecting...");
    let (mut link, device, qos) = connect(&stir, saddr)?;
    println!(
        "Found \"{}\" at 0x{:08x}, connection ESTABLISHED (conn addr 0x{:02x}).",
        device.name,
        device.address,
        link.conn()
    );
    println!(
        "  Negotiated QoS: baud={:?} max_turn={:?}ms data_size={:?} window={:?} min_turn={:?}us link_disc={:?}s",
        qos.baud, qos.max_turn_ms, qos.data_size, qos.window, qos.min_turn_us, qos.link_disc_s
    );

    println!("\nHolding link for {hold_secs}s with RR keepalives...");
    let start = Instant::now();
    let mut polls = 0u64;
    let mut replies = 0u64;
    let mut consecutive_misses = 0u64;
    let mut max_misses = 0u64;

    while start.elapsed() < Duration::from_secs(hold_secs) {
        polls += 1;
        let (responded, _) = link.transact(&stir, None)?;
        if responded {
            replies += 1;
            consecutive_misses = 0;
        } else {
            consecutive_misses += 1;
            max_misses = max_misses.max(consecutive_misses);
        }
        if polls % 10 == 0 {
            println!(
                "  {}s: {replies}/{polls} polls answered",
                start.elapsed().as_secs()
            );
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    link.disconnect(&stir);

    println!(
        "\nHeld {hold_secs}s: {replies}/{polls} RR polls answered, longest gap {max_misses} poll(s)."
    );
    if replies == 0 {
        Err("link came up (UA) but the device never answered a keepalive".into())
    } else {
        println!("M6 OK: IrLAP link established and maintained.");
        Ok(())
    }
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
    fn snrm_frame_layout() {
        let f = build_snrm(0x11223344, 0xbe68a303, 0x10);
        assert_eq!(f[0], 0xff); // caddr broadcast|cmd
        assert_eq!(f[1], 0x93); // SNRM_CMD | PF_BIT
        assert_eq!(&f[2..6], &0x11223344u32.to_le_bytes()); // saddr
        assert_eq!(&f[6..10], &0xbe68a303u32.to_le_bytes()); // daddr
        assert_eq!(f[10], 0x10); // ncaddr
        // First QoS param: baud rate PI=0x01, PL=1, PV=0x02 (9600).
        assert_eq!(&f[11..14], &[0x01, 0x01, 0x02]);
    }

    #[test]
    fn rr_and_disc_frames() {
        assert_eq!(build_rr(0x10, 0, true), vec![0x11, 0x11]); // caddr|CMD, RR|PF
        assert_eq!(build_rr(0x10, 3, false), vec![0x10, 0x71]); // vr=3 -> 3<<5|RR|PF
        assert_eq!(build_disc(0x10), vec![0x11, 0x53]); // caddr|CMD, DISC|PF
    }

    #[test]
    fn parse_ua_decodes_qos() {
        let mut u = vec![0x10, UA_RSP | PF_BIT]; // caddr, control 0x73
        u.extend_from_slice(&0xbe68a303u32.to_le_bytes()); // saddr (device)
        u.extend_from_slice(&0x11223344u32.to_le_bytes()); // daddr (us)
        u.extend_from_slice(&[PI_BAUD_RATE, 0x01, 0x02]); // 9600
        u.extend_from_slice(&[PI_MAX_TURN_TIME, 0x01, 0x01]); // 500 ms
        u.extend_from_slice(&[PI_WINDOW_SIZE, 0x01, 0x01]); // window 1
        let (saddr, _daddr, qos) = parse_ua(&u).expect("should parse");
        assert_eq!(saddr, 0xbe68a303);
        assert_eq!(qos.baud, Some(9600));
        assert_eq!(qos.max_turn_ms, Some(500));
        assert_eq!(qos.window, Some(1));
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
