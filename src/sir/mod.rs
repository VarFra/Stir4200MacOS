//! SIR (Serial Infrared) layer: async framing and FCS.
//!
//! For M1 only the FCS (`crc`) is implemented and unit-tested — it is the one
//! piece that is verifiable without hardware (brief §7). The async wrap/unwrap
//! state machine (BOF/EOF, byte stuffing, XBOFs) and the STIr4200 chip header
//! (`0x55 0xAA len len`) land at M3/M4, when they can be checked on the wire.

pub mod crc;
