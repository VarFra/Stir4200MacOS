//! IrLMP + IrTTP (TinyTP) transport and the Uwatec Smart application protocol.
//!
//! Layering (all carried in IrLAP I-frames via [`crate::irlap::IrlapLink`]):
//!   - **IrLMP** control/data PDUs (`irlmp_frame.c`): connect to LSAP 1.
//!   - **IrTTP** connect + credit-based flow control (`irttp.c`): this is what
//!     `AF_IRDA/SOCK_STREAM` gives libdivecomputer for free on Linux/Windows.
//!   - **Uwatec Smart** commands (`uwatec_smart.c`, IRDA path): once the TTP
//!     stream is up, just write `[cmd|params]` and read N bytes.
//!
//! See ANALYSIS.md §5–§6. Frame layouts/constants are taken from the Linux
//! sources; nothing is guessed.

use crate::chip::Stir;
use crate::irlap::{self, IrlapLink};

// IrLMP (`irlmp_frame.h`).
const CONTROL_BIT: u8 = 0x80;
const CONNECT_CMD: u8 = 0x01;
const CONNECT_CNF: u8 = 0x81;
const LMP_DISCONNECT: u8 = 0x02;
const LSAP_MASK: u8 = 0x7f;

// IrTTP: we connect without SAR, so the only TTP header byte we use carries the
// credit (and, on RX, the M/more bit which we ignore for a byte stream).

/// The device's dive-download service LSAP (libdivecomputer connects to 1;
/// `examples/common.c:514`).
const DEVICE_LSAP: u8 = 1;
/// Our (client) source LSAP — any free user LSAP.
const OUR_LSAP: u8 = 5;

/// Initial TTP credit we grant the device, and the level we keep it topped to.
const CREDIT_TARGET: i32 = 127;

// Uwatec Smart commands (`uwatec_smart.c:39-51`).
const CMD_MODEL: u8 = 0x10;
const CMD_HARDWARE: u8 = 0x11;
const CMD_SOFTWARE: u8 = 0x13;
const CMD_SERIAL: u8 = 0x14;
const CMD_DEVTIME: u8 = 0x1A;
const CMD_HANDSHAKE1: u8 = 0x1B;
const CMD_HANDSHAKE2: u8 = 0x1C;
const CMD_DATA: u8 = 0xC4;
const CMD_SIZE: u8 = 0xC6;
const RSP_OK: u8 = 0x01;

fn u32_le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Build an LMP CONNECT control PDU carrying a TTP connect (initial credit, no
/// SAR parameters).
fn lmp_connect(initial_credit: u8) -> Vec<u8> {
    vec![
        DEVICE_LSAP | CONTROL_BIT, // dst LSAP + control bit
        OUR_LSAP,                  // src LSAP
        CONNECT_CMD,
        0x00,                      // reserved
        initial_credit & 0x7f,     // TTP: credit, no TTP_PARAMETERS bit
    ]
}

/// Build an LMP data PDU wrapping a TTP payload.
fn lmp_data(ttp: &[u8]) -> Vec<u8> {
    let mut f = Vec::with_capacity(2 + ttp.len());
    f.push(DEVICE_LSAP); // dst LSAP, no control bit
    f.push(OUR_LSAP); // src LSAP
    f.extend_from_slice(ttp);
    f
}

/// What an incoming LMP PDU turned out to be.
enum Incoming {
    /// Connect confirmed; value is the device's granted credit for us.
    ConnectCnf(i32),
    Disconnect,
    /// Data payload (TTP header already stripped); value is credit granted to us.
    Data { data: Vec<u8>, credit: i32 },
    Ignored,
}

/// The TTP session state (credit accounting + reassembled byte stream).
pub struct Ttp {
    remote_credit: i32, // credits the device holds (granted by us)
    send_credit: i32,   // credits we hold (granted by the device)
    rx: Vec<u8>,        // reassembled application byte stream
}

impl Ttp {
    /// Parse an LMP PDU received as an IrLAP I-field.
    fn parse(&self, body: &[u8]) -> Incoming {
        if body.len() < 2 || (body[0] & LSAP_MASK) != OUR_LSAP {
            return Incoming::Ignored;
        }
        if body[0] & CONTROL_BIT != 0 {
            // Control PDU: [dst|CTL, src, opcode, rsvd, <ttp>...]
            match body.get(2).copied() {
                Some(CONNECT_CNF) => {
                    let ttp0 = body.get(4).copied().unwrap_or(0);
                    Incoming::ConnectCnf((ttp0 & 0x7f) as i32)
                }
                Some(LMP_DISCONNECT) => Incoming::Disconnect,
                _ => Incoming::Ignored,
            }
        } else {
            // Data PDU: [dst, src, ttp0, data...]
            if body.len() < 3 {
                return Incoming::Ignored;
            }
            let ttp0 = body[2];
            Incoming::Data {
                data: body[3..].to_vec(),
                credit: (ttp0 & 0x7f) as i32,
            }
        }
    }

    /// Apply a Data/ConnectCnf incoming to our credit accounting and rx stream.
    fn absorb(&mut self, inc: &Incoming) {
        match inc {
            Incoming::Data { data, credit } => {
                self.remote_credit -= 1; // device spent one of its credits
                self.send_credit += *credit; // device granted us more
                self.rx.extend_from_slice(data);
            }
            Incoming::ConnectCnf(credit) => {
                self.send_credit += *credit;
            }
            _ => {}
        }
    }

    /// Bring up the TTP connection over an established IrLAP link.
    pub fn connect(
        link: &mut IrlapLink,
        stir: &Stir,
    ) -> Result<Ttp, Box<dyn std::error::Error>> {
        let mut ttp = Ttp {
            remote_credit: CREDIT_TARGET,
            send_credit: 0,
            rx: Vec::new(),
        };

        // Send LMP CONNECT (with TTP initial credit) as an I-frame.
        let (_r, info) = link.transact(stir, Some(&lmp_connect(CREDIT_TARGET as u8)))?;
        if let Some(b) = info {
            let inc = ttp.parse(&b);
            if let Incoming::ConnectCnf(_) = inc {
                ttp.absorb(&inc);
                return Ok(ttp);
            }
            ttp.absorb(&inc);
        }

        // Otherwise poll for the CONNECT_CNF.
        for _ in 0..20 {
            let (_r, info) = link.transact(stir, None)?;
            if let Some(b) = info {
                let inc = ttp.parse(&b);
                match inc {
                    Incoming::ConnectCnf(_) => {
                        ttp.absorb(&inc);
                        return Ok(ttp);
                    }
                    Incoming::Disconnect => {
                        return Err("device refused TTP connection (disconnect)".into())
                    }
                    _ => ttp.absorb(&inc),
                }
            }
        }
        Err("no TTP CONNECT_CNF received".into())
    }

    /// Send an application payload (`[cmd|params]`) as one TTP data PDU.
    fn send(
        &mut self,
        link: &mut IrlapLink,
        stir: &Stir,
        payload: &[u8],
    ) -> Result<(), Box<dyn std::error::Error>> {
        if self.send_credit <= 0 {
            return Err("out of TTP send credit".into());
        }
        // Grant the device some credit on this frame too (delta credit in ttp0).
        let grant = (CREDIT_TARGET - self.remote_credit).clamp(0, 127);
        let mut ttp = Vec::with_capacity(1 + payload.len());
        ttp.push(grant as u8); // ttp0: M=0, delta credit
        ttp.extend_from_slice(payload);
        self.remote_credit += grant;

        let (_r, info) = link.transact(stir, Some(&lmp_data(&ttp)))?;
        self.send_credit -= 1;
        if let Some(b) = info {
            let inc = self.parse(&b);
            self.absorb(&inc);
        }
        Ok(())
    }

    /// Receive exactly `n` application bytes.
    ///
    /// The device (TTP sender) may only send while it holds credit that *we*
    /// grant. We keep its credit topped up with give-credit frames. Crucially,
    /// a give-credit frame carries no SDU and therefore does **not** consume our
    /// own send credit (`irttp.c` `irttp_give_credit`), so we can always
    /// replenish regardless of how little credit the device granted us.
    fn recv(
        &mut self,
        link: &mut IrlapLink,
        stir: &Stir,
        n: usize,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let start_len = self.rx.len();
        let mut misses = 0u32;
        let mut next_report = start_len + 16384;

        while self.rx.len() < n {
            // Keep the device's credit topped up. A give-credit I-frame both
            // grants credit and polls the secondary for its next data frame.
            let grant = (CREDIT_TARGET - self.remote_credit).clamp(0, 127);
            let (_responded, info) = if grant > 0 {
                self.remote_credit += grant;
                link.transact(stir, Some(&lmp_data(&[grant as u8])))?
            } else {
                link.transact(stir, None)?
            };

            match info {
                Some(b) => {
                    let inc = self.parse(&b);
                    if let Incoming::Disconnect = inc {
                        return Err("device disconnected during transfer".into());
                    }
                    let got_data = matches!(&inc, Incoming::Data { data, .. } if !data.is_empty());
                    self.absorb(&inc);
                    if got_data {
                        misses = 0;
                    } else {
                        misses += 1;
                    }
                }
                None => misses += 1,
            }

            if self.rx.len() >= next_report {
                println!("  received {} / {n} bytes...", self.rx.len());
                next_report += 16384;
            }

            // ~2000 empty polls (~1 minute at 9600) with no data = give up.
            if misses > 2000 {
                return Err(format!(
                    "device stopped sending after {} of {n} bytes",
                    self.rx.len()
                )
                .into());
            }
        }
        Ok(self.rx.drain(..n).collect())
    }

    /// Uwatec transfer: send `[cmd|params]`, receive `asize` bytes.
    fn transfer(
        &mut self,
        link: &mut IrlapLink,
        stir: &Stir,
        cmd: u8,
        params: &[u8],
        asize: usize,
    ) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        let mut payload = Vec::with_capacity(1 + params.len());
        payload.push(cmd);
        payload.extend_from_slice(params);
        self.send(link, stir, &payload)?;
        self.recv(link, stir, asize)
    }
}

/// M7 entry point: connect, run the Uwatec Smart handshake, read the device
/// identity and the dive memory, and save the raw dump to `out_path`.
pub fn run_download(
    vid: u16,
    pid: u16,
    speed: u32,
    out_path: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let (handle, iface) = crate::usb::open_claimed(vid, pid)?;
    println!("Claimed interface {iface}. Connecting to the dive computer...");

    let _ = handle.clear_halt(crate::chip::EP_BULK_OUT);
    let _ = handle.clear_halt(crate::chip::EP_BULK_IN);

    let stir = Stir::new(&handle);
    stir.change_speed(speed)?;

    // IrLAP connection.
    let saddr = irlap::weak_rand_pub() | 0x0000_0001;
    let (mut link, device, _qos) = irlap::connect(&stir, saddr)?;
    println!(
        "IrLAP up: \"{}\" at 0x{:08x}.",
        device.name, device.address
    );

    // IrLMP + TTP connection to LSAP 1.
    let mut ttp = Ttp::connect(&mut link, &stir)?;
    println!("TTP connected to LSAP {DEVICE_LSAP} (send credit {}).", ttp.send_credit);

    // Uwatec handshake.
    let a = ttp.transfer(&mut link, &stir, CMD_HANDSHAKE1, &[], 1)?;
    if a.first() != Some(&RSP_OK) {
        return Err(format!("handshake 1 failed (got {a:02x?})").into());
    }
    let a = ttp.transfer(&mut link, &stir, CMD_HANDSHAKE2, &[0x10, 0x27, 0, 0], 1)?;
    if a.first() != Some(&RSP_OK) {
        return Err(format!("handshake 2 failed (got {a:02x?})").into());
    }
    println!("Handshake OK.");

    // Device identity.
    let model = ttp.transfer(&mut link, &stir, CMD_MODEL, &[], 1)?[0];
    let hardware = ttp.transfer(&mut link, &stir, CMD_HARDWARE, &[], 1)?[0];
    let software = ttp.transfer(&mut link, &stir, CMD_SOFTWARE, &[], 1)?[0];
    let serial = u32_le(&ttp.transfer(&mut link, &stir, CMD_SERIAL, &[], 4)?);
    let devtime = u32_le(&ttp.transfer(&mut link, &stir, CMD_DEVTIME, &[], 4)?);
    println!(
        "Device: model=0x{model:02x} hardware=0x{hardware:02x} software=0x{software:02x} serial={serial} devtime={devtime}"
    );

    // Download all dives (fingerprint/timestamp = 0).
    let params = [0u8, 0, 0, 0, 0x10, 0x27, 0, 0];
    let length = u32_le(&ttp.transfer(&mut link, &stir, CMD_SIZE, &params, 4)?);
    println!("Dive memory size: {length} bytes.");

    if length == 0 {
        println!("No dive data on the device.");
        link.disconnect(&stir);
        return Ok(());
    }

    let total = u32_le(&ttp.transfer(&mut link, &stir, CMD_DATA, &params, 4)?);
    if total != length + 4 {
        return Err(format!("unexpected DATA size: got {total}, expected {}", length + 4).into());
    }

    println!("Downloading {length} bytes...");
    let data = ttp.recv(&mut link, &stir, length as usize)?;

    std::fs::write(out_path, &data)?;
    link.disconnect(&stir);

    println!(
        "\nM7 OK: saved {} bytes of raw dive memory to {out_path}.",
        data.len()
    );
    println!("  (Contains dives delimited by the marker A5 A5 5A 5A + 4-byte LE length.)");
    Ok(())
}
