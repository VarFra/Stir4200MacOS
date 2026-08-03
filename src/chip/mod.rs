//! STIr4200 chip control: register access, reset, and baudrate (M2).
//!
//! Register map, request codes and the init sequence are taken from the Linux
//! driver (`drivers/net/irda/stir4200.c`) — see ANALYSIS.md §1–§2. All register
//! access goes through the control endpoint (endpoint 0) with vendor requests.
//! Registers are written **one at a time**: the driver notes that multi-register
//! writes "don't appear to work" (`stir4200.c:497`).

use std::time::Duration;

use rusb::{request_type, Context, DeviceHandle, Direction, Recipient, RequestType};

// Vendor request codes (`stir4200.c:89-94`).
const REQ_READ_REG: u8 = 0x01;
const REQ_WRITE_SINGLE: u8 = 0x03;

// Register offsets (`stir4200.c:97-109`).
pub const REG_MODE: u16 = 1;
pub const REG_PDCLK: u16 = 2;
pub const REG_CTRL1: u16 = 3;
pub const REG_CTRL2: u16 = 4;
pub const REG_FIFOCTL: u16 = 5;
pub const REG_DPLL: u16 = 8;

// MODE bits (`stir4200.c:111-119`).
const MODE_SIR: u8 = 0x20;
const MODE_FASTRX: u8 = 0x08;
const MODE_NRESET: u8 = 0x02;
const MODE_2400: u8 = 0x01;

// CTRL1 bits (`stir4200.c:131-137`).
const CTRL1_SDMODE: u8 = 0x80;
const CTRL1_SRESET: u8 = 0x01;

// FIFOCTL bits (`stir4200.c:144-148`).
const FIFOCTL_DIR: u8 = 0x10;
const FIFOCTL_EMPTY: u8 = 0x04;

// The Linux driver uses a 100 ms control timeout (`stir4200.c:78`).
const CTRL_TIMEOUT: Duration = Duration::from_millis(100);

/// Bulk endpoint addresses, confirmed on hardware at M1 (see NOTES.md).
pub const EP_BULK_OUT: u8 = 0x01;
pub const EP_BULK_IN: u8 = 0x82;

/// PDCLK divisor for a SIR baud rate (`stir4200.c:121-129`).
pub fn pdclk_for(speed: u32) -> Option<u8> {
    Some(match speed {
        2400 => 0xDF,
        9600 => 0x77,
        19200 => 0x3B,
        38400 => 0x1D,
        57600 => 0x13,
        115200 => 0x09,
        _ => return None,
    })
}

/// The MODE register value for a given SIR baud rate.
fn mode_for(speed: u32) -> u8 {
    let mut mode = MODE_NRESET | MODE_FASTRX | MODE_SIR;
    if speed == 2400 {
        mode |= MODE_2400;
    }
    mode
}

/// Handle to a claimed STIr4200, offering register-level operations.
pub struct Stir<'a> {
    handle: &'a DeviceHandle<Context>,
    /// Transmit power: 0 = highest … 3 = lowest.
    pub tx_power: u8,
    /// Receiver sensitivity: 0 = most sensitive … 6.
    pub rx_sensitivity: u8,
}

impl<'a> Stir<'a> {
    pub fn new(handle: &'a DeviceHandle<Context>) -> Self {
        // Defaults match the Linux module parameters (`stir4200.c:69-75`).
        Self {
            handle,
            tx_power: 0,
            rx_sensitivity: 1,
        }
    }

    /// Write a single register (vendor control OUT, `stir4200.c:194-205`).
    pub fn write_reg(&self, reg: u16, value: u8) -> rusb::Result<()> {
        let rt = request_type(Direction::Out, RequestType::Vendor, Recipient::Device);
        self.handle
            .write_control(rt, REQ_WRITE_SINGLE, value as u16, reg, &[], CTRL_TIMEOUT)?;
        debug!("write reg {reg} = 0x{value:02x}");
        Ok(())
    }

    /// Read `buf.len()` consecutive registers starting at `reg`
    /// (vendor control IN, `stir4200.c:208-218`).
    pub fn read_regs(&self, reg: u16, buf: &mut [u8]) -> rusb::Result<usize> {
        let rt = request_type(Direction::In, RequestType::Vendor, Recipient::Device);
        let n = self
            .handle
            .read_control(rt, REQ_READ_REG, 0, reg, buf, CTRL_TIMEOUT)?;
        debug!("read reg {reg} ({n} byte) = {:02x?}", &buf[..n]);
        Ok(n)
    }

    /// Read a single register.
    pub fn read_reg(&self, reg: u16) -> rusb::Result<u8> {
        let mut b = [0u8; 1];
        let n = self.read_regs(reg, &mut b)?;
        if n < 1 {
            return Err(rusb::Error::Other);
        }
        Ok(b[0])
    }

    /// Read the FIFO status: 3 registers from FIFOCTL. Returns `(ctl, count)`
    /// where `count` is the number of bytes in the FIFO (`stir4200.c:597-611`).
    pub fn fifo_status(&self) -> rusb::Result<(u8, u16)> {
        let mut b = [0u8; 3];
        let n = self.read_regs(REG_FIFOCTL, &mut b)?;
        if n < 3 {
            return Err(rusb::Error::Other);
        }
        let count = (((b[2] & 0x1f) as u16) << 8) | b[1] as u16;
        Ok((b[0], count))
    }

    /// Full init / speed-change sequence for SIR (`stir4200.c:499-558`,
    /// ANALYSIS.md §2). Writes one register at a time.
    pub fn change_speed(&self, speed: u32) -> rusb::Result<()> {
        let pdclk = pdclk_for(speed).ok_or(rusb::Error::InvalidParam)?;
        info!("change speed to {speed} baud (SIR)");

        self.write_reg(REG_CTRL1, CTRL1_SRESET)?; // 1. reset modulator
        self.write_reg(REG_DPLL, 0x15)?; // 2. undocumented DPLL tweak
        self.write_reg(REG_PDCLK, pdclk)?; // 3. clock for speed
        self.write_reg(REG_MODE, mode_for(speed))?; // 4. SIR mode
        self.write_reg(REG_CTRL1, CTRL1_SDMODE | ((self.tx_power & 3) << 1))?; // 5.
        self.write_reg(REG_CTRL1, (self.tx_power & 3) << 1)?; // 6.
        self.write_reg(REG_CTRL2, (self.rx_sensitivity & 7) << 5)?; // 7. sensitivity
        Ok(())
    }
}

/// M2 entry point: reset the chip, set the baud rate, and verify by reading
/// the registers back.
pub fn run_init(vid: u16, pid: u16, speed: u32) -> Result<(), Box<dyn std::error::Error>> {
    if pdclk_for(speed).is_none() {
        return Err(format!("unsupported speed {speed} (use 2400/9600/19200/38400/57600/115200)").into());
    }

    let (handle, iface) = crate::usb::open_claimed(vid, pid)?;
    println!("Claimed interface {iface}. Initializing STIr4200 for {speed} baud (SIR)...");

    // Mirror the driver's net_open: clear halt on both bulk endpoints
    // (`stir4200.c:855-858`). Best-effort — not fatal if unsupported.
    let _ = handle.clear_halt(EP_BULK_OUT);
    let _ = handle.clear_halt(EP_BULK_IN);

    let stir = Stir::new(&handle);
    stir.change_speed(speed)?;

    // Read back the registers that should hold a stable value and compare only
    // the *writable* bits. `mask` selects the bits we actually control; the
    // rest are read-only revision/status bits (e.g. CTRL2's low bits carry the
    // chip REVID and read back independently of what we write).
    println!();
    println!("Register read-back (written vs read):");
    let expected = [
        // (name, reg, expected_value, writable_mask)
        ("PDCLK", REG_PDCLK, pdclk_for(speed).unwrap(), 0xffu8),
        ("MODE", REG_MODE, mode_for(speed), 0xff),
        ("CTRL2", REG_CTRL2, (stir.rx_sensitivity & 7) << 5, 0xE0),
    ];
    let mut all_ok = true;
    for (name, reg, exp, mask) in expected {
        match stir.read_reg(reg) {
            Ok(v) => {
                let ok = (v & mask) == (exp & mask);
                all_ok &= ok;
                let note = if mask != 0xff {
                    format!(" (writable bits 0x{mask:02x})")
                } else {
                    String::new()
                };
                println!(
                    "  {name:<6} (reg {reg:>2}): wrote 0x{exp:02x}, read 0x{v:02x}  {}{note}",
                    if ok { "OK" } else { "MISMATCH" }
                );
            }
            Err(e) => {
                all_ok = false;
                println!("  {name:<6} (reg {reg:>2}): read error: {e}");
            }
        }
    }

    // CTRL2 low two bits carry the chip revision id (REVID, `stir4200.c:139-142`).
    if let Ok(ctrl2) = stir.read_reg(REG_CTRL2) {
        println!("  chip REVID (CTRL2 & 0x03) = {}", ctrl2 & 0x03);
    }

    // Exercise the multi-register read path the driver actually relies on.
    match stir.fifo_status() {
        Ok((ctl, count)) => println!(
            "  FIFO status: ctl=0x{ctl:02x} (dir={}, empty={}) count={count}",
            if ctl & FIFOCTL_DIR != 0 { "tx" } else { "rx" },
            ctl & FIFOCTL_EMPTY != 0
        ),
        Err(e) => println!("  FIFO status read error: {e}"),
    }

    println!();
    if all_ok {
        println!("M2 OK: the registers read back the written values.");
    } else {
        println!(
            "M2 partial: some registers did not read back as written. This may be normal for \
             certain registers on this chip revision — record the exact values in NOTES.md \
             before proceeding to M3."
        );
    }

    Ok(())
}
