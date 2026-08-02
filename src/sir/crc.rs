//! IrDA SIR frame check sequence (FCS).
//!
//! This is the CRC used by the async SIR wrapper (Linux `net/irda/wrapper.c`,
//! `include/net/irda/crc.h`). It is **not** the "standard" CRC-16: it is
//! CRC-CCITT with the *reflected* polynomial `0x8408` (i.e. poly `0x1021`,
//! refin/refout), initial value `0xFFFF`, and the transmitted FCS is the
//! ones' complement of the running value, sent LSB-first. Appending the
//! transmitted FCS and running the CRC over data+FCS yields the residue
//! `GOOD_FCS = 0xF0B8`.
//!
//! Reference: `include/net/irda/crc.h`:
//!   `#define INIT_FCS 0xffff` · `#define GOOD_FCS 0xf0b8`
//!   `#define irda_fcs(fcs, c) crc_ccitt_byte(fcs, c)`

/// Initial FCS value (Linux `INIT_FCS`).
pub const INIT_FCS: u16 = 0xffff;

/// Residue after running the CRC over a frame that includes a valid
/// (complemented, LSB-first) FCS. Linux `GOOD_FCS`.
pub const GOOD_FCS: u16 = 0xf0b8;

/// Reflected CRC-CCITT polynomial (0x1021 bit-reversed).
const POLY: u16 = 0x8408;

/// Update the running FCS with one more byte (Linux `crc_ccitt_byte`).
#[inline]
pub fn crc_ccitt_byte(mut crc: u16, byte: u8) -> u16 {
    crc ^= byte as u16;
    for _ in 0..8 {
        if crc & 1 != 0 {
            crc = (crc >> 1) ^ POLY;
        } else {
            crc >>= 1;
        }
    }
    crc
}

/// Update the running FCS with a slice of bytes.
#[inline]
pub fn crc_ccitt(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc = crc_ccitt_byte(crc, b);
    }
    crc
}

/// Compute the FCS to transmit for `data`: the ones' complement of the running
/// CRC (from `INIT_FCS`), returned as the two little-endian bytes `[lo, hi]`
/// that go on the wire after the payload.
#[inline]
pub fn fcs_bytes(data: &[u8]) -> [u8; 2] {
    let fcs = !crc_ccitt(INIT_FCS, data);
    [(fcs & 0xff) as u8, (fcs >> 8) as u8]
}

/// Verify a received frame body that still has its two FCS bytes appended.
/// Returns true iff the FCS checks out (residue == `GOOD_FCS`).
#[inline]
pub fn check(frame_with_fcs: &[u8]) -> bool {
    crc_ccitt(INIT_FCS, frame_with_fcs) == GOOD_FCS
}

#[cfg(test)]
mod tests {
    use super::*;

    // Canonical CRC-CCITT (reflected, "CRC-16/X-25" family) check string.
    // The running value before complement is 0x6F91; the complemented,
    // transmitted value is 0x906E (the well-known X-25 "check" value).
    #[test]
    fn known_vector_123456789() {
        assert_eq!(crc_ccitt(INIT_FCS, b"123456789"), 0x6F91);
        assert_eq!(fcs_bytes(b"123456789"), [0x6E, 0x90]);
    }

    #[test]
    fn empty_input_is_init() {
        assert_eq!(crc_ccitt(INIT_FCS, b""), INIT_FCS);
    }

    // The defining property used by the SIR de-wrapper: run the CRC over the
    // payload plus its transmitted FCS and you must land on GOOD_FCS.
    #[test]
    fn good_fcs_residue_holds() {
        for payload in [
            &b""[..],
            &b"1"[..],
            &b"123456789"[..],
            &[0x00, 0xff, 0x7d, 0xc0, 0xc1][..], // includes control/escape byte values
        ] {
            let mut framed = payload.to_vec();
            framed.extend_from_slice(&fcs_bytes(payload));
            assert_eq!(
                crc_ccitt(INIT_FCS, &framed),
                GOOD_FCS,
                "residue mismatch for payload {payload:02x?}"
            );
            assert!(check(&framed));
        }
    }

    #[test]
    fn corrupted_frame_fails_check() {
        let payload = b"hello galileo";
        let mut framed = payload.to_vec();
        framed.extend_from_slice(&fcs_bytes(payload));
        framed[3] ^= 0x01; // flip one bit
        assert!(!check(&framed));
    }
}
