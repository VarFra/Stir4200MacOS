//! IrDA SIR async framing: wrapping (TX) and de-wrapping (RX).
//!
//! Mirrors the Linux `net/irda/wrapper.c` state machine (ANALYSIS.md §4).
//! Frame on the wire (before the STIr4200 chip header, which lives in `chip`):
//!
//! ```text
//! [ XBOF × N (0xFF) ] [ BOF 0xC0 ] [ stuffed(payload) ] [ stuffed(FCS_lo,FCS_hi) ] [ EOF 0xC1 ]
//! ```
//!
//! Byte stuffing: any `BOF`/`EOF`/`CE` byte becomes `CE (0x7D)` followed by
//! `byte ^ 0x20`. The FCS is the SIR CRC (see [`super::crc`]).

use super::crc;

/// Beginning-of-frame flag.
pub const BOF: u8 = 0xC0;
/// End-of-frame flag.
pub const EOF: u8 = 0xC1;
/// Control-escape byte.
pub const CE: u8 = 0x7D;
/// Extra beginning-of-frame (preamble) byte.
pub const XBOF: u8 = 0xFF;
/// Transparency modifier applied to stuffed bytes.
pub const IRDA_TRANS: u8 = 0x20;

/// Default number of preamble XBOFs for user-generated frames
/// (`wrapper.c:110`, the "wrong magic" path used outside IrLAP).
pub const DEFAULT_XBOFS: usize = 10;

/// Stuff one byte into `out` (`wrapper.c:58-75`).
fn stuff(out: &mut Vec<u8>, b: u8) {
    if b == BOF || b == EOF || b == CE {
        out.push(CE);
        out.push(b ^ IRDA_TRANS);
    } else {
        out.push(b);
    }
}

/// Wrap `payload` into a complete async SIR frame (with `xbofs` preamble bytes,
/// BOF, stuffed payload+FCS, EOF). Does **not** add the STIr4200 chip header.
pub fn async_wrap(payload: &[u8], xbofs: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(payload.len() + xbofs + 8);
    out.resize(xbofs, XBOF);
    out.push(BOF);
    for &b in payload {
        stuff(&mut out, b);
    }
    let fcs = crc::fcs_bytes(payload);
    stuff(&mut out, fcs[0]);
    stuff(&mut out, fcs[1]);
    out.push(EOF);
    out
}

/// Receiver state for the de-wrap machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Outside,
    Begin,
    Inside,
    Escape,
}

/// Streaming SIR de-wrapper: feed bytes as they arrive; each completed, valid
/// frame is returned as its payload (FCS stripped). Malformed frames are
/// dropped and counted, never returned (mirrors `async_unwrap_char`).
#[derive(Debug, Default)]
pub struct Unwrapper {
    state: State,
    buf: Vec<u8>,
    /// Frames dropped because the FCS did not check out.
    pub crc_errors: u64,
    /// Frames dropped because they were too short to hold an FCS.
    pub short_frames: u64,
}

impl Unwrapper {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed one received byte. Returns `Some(payload)` when a valid frame
    /// completes on this byte.
    pub fn push(&mut self, byte: u8) -> Option<Vec<u8>> {
        match byte {
            BOF => {
                // (Re)start a frame.
                self.state = State::Begin;
                self.buf.clear();
                None
            }
            CE => {
                if self.state != State::Outside {
                    self.state = State::Escape;
                }
                None
            }
            EOF => self.finish(),
            _ => {
                match self.state {
                    State::Outside => {} // garbage between frames
                    State::Escape => {
                        self.buf.push(byte ^ IRDA_TRANS);
                        self.state = State::Inside;
                    }
                    State::Begin | State::Inside => {
                        self.buf.push(byte);
                        self.state = State::Inside;
                    }
                }
                None
            }
        }
    }

    /// Feed a slice, collecting all completed frames.
    pub fn push_all(&mut self, bytes: &[u8]) -> Vec<Vec<u8>> {
        let mut frames = Vec::new();
        for &b in bytes {
            if let Some(f) = self.push(b) {
                frames.push(f);
            }
        }
        frames
    }

    fn finish(&mut self) -> Option<Vec<u8>> {
        let prev = self.state;
        self.state = State::Outside;
        if prev == State::Outside {
            return None; // stray EOF
        }
        if self.buf.len() < 2 {
            self.short_frames += 1;
            self.buf.clear();
            return None;
        }
        // buf holds payload + FCS; a good frame yields the GOOD_FCS residue.
        if crc::check(&self.buf) {
            let payload = self.buf[..self.buf.len() - 2].to_vec();
            self.buf.clear();
            Some(payload)
        } else {
            self.crc_errors += 1;
            self.buf.clear();
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_structure() {
        let w = async_wrap(b"AB", DEFAULT_XBOFS);
        assert!(w[..DEFAULT_XBOFS].iter().all(|&b| b == XBOF));
        assert_eq!(w[DEFAULT_XBOFS], BOF);
        assert_eq!(*w.last().unwrap(), EOF);
    }

    #[test]
    fn stuffing_of_control_bytes() {
        // A payload byte equal to EOF must be escaped as CE, EOF^0x20.
        let w = async_wrap(&[EOF], 0);
        assert_eq!(w[0], BOF);
        assert_eq!(w[1], CE);
        assert_eq!(w[2], EOF ^ IRDA_TRANS); // 0xE1
    }

    #[test]
    fn roundtrip_including_control_bytes() {
        let payloads: &[&[u8]] = &[
            b"",
            b"A",
            b"hello galileo",
            &[0x00, 0xC0, 0xC1, 0x7D, 0xFF, 0x55, 0xAA],
            &[0x1B],       // Uwatec HANDSHAKE1 command byte
        ];
        for &p in payloads {
            let wire = async_wrap(p, DEFAULT_XBOFS);
            let mut u = Unwrapper::new();
            let frames = u.push_all(&wire);
            assert_eq!(frames, vec![p.to_vec()], "roundtrip failed for {p:02x?}");
            assert_eq!(u.crc_errors, 0);
        }
    }

    #[test]
    fn ignores_garbage_and_multiple_bofs() {
        let mut u = Unwrapper::new();
        let mut wire = vec![0x11, 0x22, XBOF, XBOF, BOF, BOF]; // garbage + extra BOF
        wire.extend_from_slice(&async_wrap(b"OK", 0)[1..]); // frame without its own leading BOF
        let frames = u.push_all(&wire);
        assert_eq!(frames, vec![b"OK".to_vec()]);
    }

    #[test]
    fn corrupted_frame_is_dropped() {
        let mut wire = async_wrap(b"payload", 4);
        // Corrupt a payload byte after the BOF.
        wire[6] ^= 0x01;
        let mut u = Unwrapper::new();
        let frames = u.push_all(&wire);
        assert!(frames.is_empty());
        assert_eq!(u.crc_errors, 1);
    }
}
