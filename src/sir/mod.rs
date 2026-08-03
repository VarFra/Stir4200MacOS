//! SIR (Serial Infrared) layer: async framing and FCS.
//!
//! - `crc`: the SIR frame check sequence (unit-tested, hardware-free).
//! - `framing`: async wrap (TX, used at M3) and the de-wrap state machine
//!   (RX, wired at M4). Both are unit-tested via round-trip.
//!
//! The STIr4200 chip header (`0x55 0xAA len len`) is added in the `chip` layer,
//! since it is specific to this dongle, not to SIR framing in general.

pub mod crc;
pub mod framing;
