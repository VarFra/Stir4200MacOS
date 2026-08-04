//! Parse a raw Uwatec Galileo/Trimix memory dump into dives and export
//! Subsurface-native XML (M8).
//!
//! The dive-record layout, header offsets, the sample bit-stream and every
//! scaling constant are taken from libdivecomputer's `uwatec_smart_parser.c`
//! (and `uwatec_smart.c` for the record framing). This covers the Galileo
//! family in scope (Sol / Luna = model 0x11, Trimix = 0x19): both use the same
//! sample table, and the per-dive header layout is chosen at runtime from
//! `data[43] & 0x80`, exactly as libdivecomputer does.

use std::fmt::Write as _;

const MARKER: [u8; 4] = [0xa5, 0xa5, 0x5a, 0x5a];
const BAR: f64 = 100000.0;
const FRESH: f64 = 1000.0;
const SALT: f64 = 1025.0;
const EPOCH: i64 = 946_684_800; // 2000-01-01 00:00:00 UTC

// Settings bits (`uwatec_smart_parser.c:70-72`).
const FREEDIVE: u32 = 0x0000_0080;
const GAUGE: u32 = 0x0000_1000;
const SALINITY: u32 = 0x0010_0000;

#[derive(Clone, Copy, PartialEq)]
enum SampleType {
    Rbt,
    Temperature,
    Pressure,
    Depth,
    Heartrate,
    Bearing,
    Alarms,
    Time,
    Apnea,
    Misc,
}
use SampleType::*;

/// One entry of the Galileo sample table: (type, absolute, index, ntypebits,
/// ignoretype, extrabytes) — `uwatec_smart_galileo_samples`. `index` (the tank
/// / alarm-group selector) is kept to mirror the source table 1:1 even though
/// this simplified parser does not split tanks or decode alarm events.
#[allow(dead_code)]
struct SInfo(SampleType, bool, u32, u32, bool, u32);

const GALILEO_SAMPLES: [SInfo; 19] = [
    SInfo(Depth, false, 0, 1, false, 0),       // 0ddd dddd
    SInfo(Rbt, false, 0, 3, false, 0),         // 100d dddd
    SInfo(Pressure, false, 0, 4, false, 0),    // 1010 dddd
    SInfo(Temperature, false, 0, 4, false, 0), // 1011 dddd
    SInfo(Time, true, 0, 4, false, 0),         // 1100 dddd
    SInfo(Heartrate, false, 0, 4, false, 0),   // 1101 dddd
    SInfo(Alarms, true, 0, 4, false, 0),       // 1110 dddd
    SInfo(Alarms, true, 1, 8, false, 1),       // 1111 0000 dddddddd
    SInfo(Depth, true, 0, 8, false, 2),        // 1111 0001 + 2
    SInfo(Rbt, true, 0, 8, false, 1),          // 1111 0010 + 1
    SInfo(Temperature, true, 0, 8, false, 2),  // 1111 0011 + 2
    SInfo(Pressure, true, 0, 8, false, 2),     // 1111 0100 + 2
    SInfo(Pressure, true, 1, 8, false, 2),     // 1111 0101 + 2
    SInfo(Pressure, true, 2, 8, false, 2),     // 1111 0110 + 2
    SInfo(Heartrate, true, 0, 8, false, 1),    // 1111 0111 + 1
    SInfo(Bearing, true, 0, 8, false, 2),      // 1111 1000 + 2
    SInfo(Alarms, true, 2, 8, false, 1),       // 1111 1001 + 1
    SInfo(Apnea, true, 0, 8, false, 0),        // 1111 1010
    SInfo(Misc, true, 0, 8, false, 1),         // 1111 1011
];

/// Galileo type-byte classifier (`uwatec_galileo_identify`).
fn galileo_identify(value: u8) -> usize {
    if value & 0x80 == 0 {
        0
    } else if value & 0xE0 == 0x80 {
        1
    } else if value & 0xF0 != 0xF0 {
        ((value & 0x70) >> 4) as usize
    } else {
        (value & 0x0F) as usize + 7
    }
}

/// Sign-extend the low `nbits` of `value` (`array.c` `signextend`).
fn signextend(value: u32, nbits: u32) -> i32 {
    if nbits == 0 || nbits > 32 {
        return 0;
    }
    let signbit = 1u32 << (nbits - 1);
    let mask = signbit - 1;
    if value & signbit == signbit {
        (value | !mask) as i32
    } else {
        (value & mask) as i32
    }
}

fn u16_le(d: &[u8], off: usize) -> u16 {
    u16::from_le_bytes([d[off], d[off + 1]])
}
fn u32_le(d: &[u8], off: usize) -> u32 {
    u32::from_le_bytes([d[off], d[off + 1], d[off + 2], d[off + 3]])
}

#[derive(Clone, Copy)]
struct Sample {
    time: u32,           // seconds
    depth: f64,          // metres
    temp: Option<f64>,   // °C
    pressure: Option<f64>, // bar
}

struct Dive {
    unix_time: i64,     // device local wall-clock as a unix timestamp
    divetime_s: u32,
    maxdepth: f64,
    temp_min: f64,
    salinity_gl: u32,   // water salinity in g/l (1000 fresh, 1025 salt)
    cylinders: Vec<Cylinder>,
    gas_changes: Vec<(u32, usize, f64, f64)>, // (time_s, cylinder, o2, he)
    samples: Vec<Sample>,
}

#[derive(Clone, Copy)]
struct Cylinder {
    o2: f64,        // fraction
    he: f64,        // fraction
    begin_bar: f64, // 0 = unknown
    end_bar: f64,   // 0 = unknown
}

/// Header-offset table (subset we need).
struct Header {
    size: usize,
    maxdepth: usize,
    divetime: usize,
    temp_minimum: usize,
    timezone: usize,
    settings: usize,
    gasmix: Option<usize>,
    tankpressure: Option<usize>,
    ngases: usize,
}

/// Split the dump into dive records, newest first (mirrors
/// `uwatec_smart_extract_dives`), returned oldest-first.
fn extract_dives(data: &[u8]) -> Vec<&[u8]> {
    let mut records = Vec::new();
    let size = data.len();
    let mut previous = size;
    let mut current = if size >= 4 { size - 4 } else { 0 };
    while current > 0 {
        current -= 1;
        if data[current..current + 4] == MARKER {
            let len = u32_le(data, current + 4) as usize;
            if len < 8 || current + len > previous {
                break; // malformed
            }
            records.push(&data[current..current + len]);
            previous = current;
            current = if current >= 4 { current - 4 } else { 0 };
        }
    }
    records.reverse();
    records
}

/// Parse one dive record.
fn parse_dive(rec: &[u8]) -> Result<Dive, String> {
    if rec.len() < 44 {
        return Err("dive record too short".into());
    }

    // Per-dive header selection (Galileo family): data[43] & 0x80 => trimix.
    let trimix = rec[43] & 0x80 != 0;
    let header = if trimix {
        Header {
            size: 84,
            maxdepth: 22,
            divetime: 26,
            temp_minimum: 30,
            timezone: 16,
            settings: 68,
            gasmix: None,
            tankpressure: None,
            ngases: 0,
        }
    } else {
        Header {
            size: 152,
            maxdepth: 22,
            divetime: 26,
            temp_minimum: 30,
            timezone: 16,
            settings: 92,
            gasmix: Some(44),
            tankpressure: Some(50),
            ngases: 3,
        }
    };

    if rec.len() < header.size {
        return Err(format!(
            "dive record shorter ({}) than header ({})",
            rec.len(),
            header.size
        ));
    }

    // Date/time: timestamp at offset 8 (half-seconds since EPOCH), plus the
    // device timezone (units of 15 min).
    let timestamp = u32_le(rec, 8) as i64;
    let tz_offset_s = (rec[header.timezone] as i8) as i64 * 900;
    let unix_time = EPOCH + timestamp / 2 + tz_offset_s;

    let divetime_s = u16_le(rec, header.divetime) as u32 * 60;

    // Settings: water type + dive mode.
    let settings = u32_le(rec, header.settings);
    let salt = settings & SALINITY != 0;
    let density = if salt { SALT } else { FRESH };
    let freedive = settings & FREEDIVE != 0;
    let gauge = settings & GAUGE != 0;

    let maxdepth = u16_le(rec, header.maxdepth) as f64 * (BAR / 1000.0) / (density * 10.0);
    let temp_min = (u16_le(rec, header.temp_minimum) as i16) as f64 / 10.0;

    // Gas mixes. Galileo-mode dives carry them (and tank pressures) in the
    // header; trimix-mode dives report them via MISC samples (parsed in the loop
    // below). Tracked as (mixid, o2, he, begin_bar, end_bar), deduped by mixid.
    // Tank pressures are raw/128 bar (`uwatec_smart_parser.c:802`); 0/0xFFFF
    // mean "unknown".
    let mut mixes: Vec<(u32, f64, f64, f64, f64)> = Vec::new();
    let pbar = |raw: u16| -> f64 {
        if raw == 0 || raw == 0xFFFF {
            0.0
        } else {
            raw as f64 / 128.0
        }
    };
    if let Some(gm) = header.gasmix {
        for i in 0..header.ngases {
            let o2 = u16_le(rec, gm + i * 2);
            if o2 != 0 {
                // Galileo tank-pressure layout (`uwatec_smart_parser.c:540`):
                // end at tp+2i, begin at tp+2i+2*ngases.
                let (mut begin, mut end) = (0.0, 0.0);
                if !freedive {
                    if let Some(tp) = header.tankpressure {
                        end = pbar(u16_le(rec, tp + 2 * i));
                        begin = pbar(u16_le(rec, tp + 2 * i + 2 * header.ngases));
                    }
                }
                mixes.push((i as u32, o2 as f64 / 100.0, 0.0, begin, end));
            }
        }
    }

    // ---- Sample bit-stream ----
    let interval: u32 = if freedive { 1 } else { 4 };
    let _ = gauge;

    let mut time = 0u32;
    let mut depth = 0i64;
    let mut depth_calibration = 0i64;
    let mut calibrated = false;
    let mut temperature = 0i32;
    let mut pressure = 0i64;
    let mut have_depth = false;
    let mut have_temperature = false;
    let mut have_pressure = false;

    // Active gas mix tracking (mix id), for emitting gas-change events.
    let mut active_gasmix: u32 = 0;
    let mut prev_gasmix: i64 = -1;
    let mut gas_changes: Vec<(u32, usize, f64, f64)> = Vec::new(); // (time, cyl, o2, he)

    let mut samples = Vec::new();
    let mut offset = header.size;
    let n = rec.len();

    while offset < n {
        let id = galileo_identify(rec[offset]);
        if id >= GALILEO_SAMPLES.len() {
            return Err(format!("invalid sample type bits 0x{:02x}", rec[offset]));
        }
        let info = &GALILEO_SAMPLES[id];

        // Consume full type bytes.
        offset += (info.3 / 8) as usize;

        // Remaining data bits in the last type byte.
        let mut nbits = 0u32;
        let mut value: u32 = 0;
        let rem = info.3 % 8;
        if rem > 0 {
            if offset >= n {
                break;
            }
            nbits = 8 - rem;
            value = (rec[offset] & (0xFF >> rem)) as u32;
            if info.4 {
                nbits = 0;
                value = 0;
            }
            offset += 1;
        }

        if offset + info.5 as usize > n {
            break; // incomplete
        }
        for _ in 0..info.5 {
            nbits += 8;
            value = (value << 8) + rec[offset] as u32;
            offset += 1;
        }

        let svalue = signextend(value, nbits);
        let mut complete: u32 = 0;

        match info.0 {
            Depth => {
                if info.1 {
                    depth = value as i64;
                    if !calibrated {
                        calibrated = true;
                        depth_calibration = depth;
                    }
                    have_depth = true;
                } else {
                    depth += svalue as i64;
                }
                complete = 1;
            }
            Temperature => {
                if info.1 {
                    temperature = svalue;
                    have_temperature = true;
                } else {
                    temperature += svalue;
                }
            }
            Pressure => {
                if info.1 {
                    // Absolute pressure carries the active tank/gas: for trimix
                    // it is packed in the top nibble, otherwise it is the
                    // sample's table index (`uwatec_smart_parser.c:1014`).
                    let tank = if trimix {
                        (value & 0xF000) >> 12
                    } else {
                        info.2
                    };
                    if trimix {
                        pressure = (value & 0x0FFF) as i64;
                    } else {
                        pressure = value as i64;
                    }
                    have_pressure = true;
                    active_gasmix = tank;
                } else {
                    pressure += svalue as i64;
                }
            }
            Alarms => {
                // Only the EV_GASMIX field interests us (`galileo_events_1`,
                // `trimix_events_2`).
                match info.2 {
                    1 => active_gasmix = (value & 0x60) >> 5,
                    2 if trimix => active_gasmix = (value & 0xF0) >> 4,
                    _ => {}
                }
            }
            Time => complete = value,
            Apnea => offset += 8,
            Misc => {
                // MISC payload: [subtype][.. n-1 bytes ..]; `value` = n.
                let len = value as usize;
                if len < 1 || offset + len - 1 > n {
                    break; // incomplete
                }
                let subtype = rec[offset];
                // subtypes 32..=41 describe gas mix `subtype-32` (o2, he, and
                // begin/end tank pressures) — `uwatec_smart_parser.c:1095`.
                if (32..=41).contains(&subtype) && len >= 16 {
                    let mixid = (subtype - 32) as u32;
                    let o2 = u16_le(rec, offset + 1);
                    let he = u16_le(rec, offset + 3);
                    let begin = pbar(u16_le(rec, offset + 5));
                    let end = pbar(u16_le(rec, offset + 7));
                    if (o2 != 0 || he != 0) && !mixes.iter().any(|m| m.0 == mixid) {
                        mixes.push((mixid, o2 as f64 / 100.0, he as f64 / 100.0, begin, end));
                    }
                }
                offset += len - 1;
            }
            // Rbt / Heartrate / Bearing / Alarms: consumed, no profile point.
            _ => {}
        }

        while complete > 0 {
            // Emit a gas-change event when the active mix changes.
            if !mixes.is_empty() && active_gasmix as i64 != prev_gasmix {
                if let Some(ci) = mixes.iter().position(|m| m.0 == active_gasmix) {
                    gas_changes.push((time, ci, mixes[ci].1, mixes[ci].2));
                }
                prev_gasmix = active_gasmix as i64;
            }

            let depth_m =
                (depth - depth_calibration) as f64 * (2.0 * BAR / 1000.0) / (density * 10.0);
            samples.push(Sample {
                time,
                depth: if have_depth { depth_m } else { 0.0 },
                temp: if have_temperature {
                    Some(temperature as f64 / 2.5)
                } else {
                    None
                },
                pressure: if have_pressure {
                    Some(pressure as f64 / 4.0)
                } else {
                    None
                },
            });
            time += interval;
            complete -= 1;
        }
    }

    // Re-map gas-change cylinder indices to the sorted cylinder order.
    let order: Vec<u32> = {
        let mut ids: Vec<u32> = mixes.iter().map(|m| m.0).collect();
        ids.sort_unstable();
        ids
    };
    let remap = |old_ci: usize| -> usize {
        let mixid = mixes[old_ci].0;
        order.iter().position(|&x| x == mixid).unwrap_or(old_ci)
    };
    let gas_changes = gas_changes
        .into_iter()
        .map(|(t, ci, o2, he)| (t, remap(ci), o2, he))
        .collect();

    mixes.sort_by_key(|m| m.0);
    let cylinders = mixes
        .iter()
        .map(|m| Cylinder {
            o2: m.1,
            he: m.2,
            begin_bar: m.3,
            end_bar: m.4,
        })
        .collect();

    Ok(Dive {
        unix_time,
        divetime_s,
        maxdepth,
        temp_min,
        salinity_gl: if salt { 1025 } else { 1000 },
        cylinders,
        gas_changes,
        samples,
    })
}

/// Civil date/time (UTC of the given unix seconds) — Howard Hinnant's algorithm.
fn civil(secs: i64) -> (i64, u32, u32, u32, u32, u32) {
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (hh, mm, ss) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = if m <= 2 { y + 1 } else { y };
    (year, m as u32, d as u32, hh as u32, mm as u32, ss as u32)
}

/// Write a Subsurface-native XML divelog for the parsed dives.
fn write_xml(dives: &[Dive], model_name: &str) -> String {
    let mut out = String::new();
    out.push_str("<divelog program='stir4200' version='3'>\n<dives>\n");

    for (i, dive) in dives.iter().enumerate() {
        let (y, mo, d, hh, mm, ss) = civil(dive.unix_time);
        let dur_m = dive.divetime_s / 60;
        let dur_s = dive.divetime_s % 60;

        let _ = write!(
            out,
            "<dive number='{}' date='{:04}-{:02}-{:02}' time='{:02}:{:02}:{:02}' duration='{}:{:02} min'>\n",
            i + 1,
            y,
            mo,
            d,
            hh,
            mm,
            ss,
            dur_m,
            dur_s
        );

        for (ci, cyl) in dive.cylinders.iter().enumerate() {
            let _ = write!(out, "  <cylinder o2='{:.1}%'", cyl.o2 * 100.0);
            if cyl.he > 0.0 {
                let _ = write!(out, " he='{:.1}%'", cyl.he * 100.0);
            }
            let _ = write!(out, " description='mix {}'", ci + 1);
            if cyl.begin_bar > 0.0 {
                let _ = write!(out, " start='{:.0} bar'", cyl.begin_bar);
            }
            if cyl.end_bar > 0.0 {
                let _ = write!(out, " end='{:.0} bar'", cyl.end_bar);
            }
            out.push_str(" />\n");
        }

        let _ = write!(
            out,
            "  <divecomputer model='{}' salinity='{} g/l'>\n    <depth max='{:.2} m' />\n    <temperature water='{:.1} C' />\n",
            xml_escape(model_name),
            dive.salinity_gl,
            dive.maxdepth,
            dive.temp_min
        );

        for (t, cyl, o2, he) in &dive.gas_changes {
            let _ = write!(
                out,
                "    <event time='{}:{:02} min' type='25' name='gaschange' cylinder='{}' o2='{:.1}%'",
                t / 60,
                t % 60,
                cyl,
                o2 * 100.0
            );
            if *he > 0.0 {
                let _ = write!(out, " he='{:.1}%'", he * 100.0);
            }
            out.push_str(" />\n");
        }

        for s in &dive.samples {
            let _ = write!(
                out,
                "    <sample time='{}:{:02} min' depth='{:.2} m'",
                s.time / 60,
                s.time % 60,
                s.depth.max(0.0)
            );
            if let Some(t) = s.temp {
                let _ = write!(out, " temp='{t:.1} C'");
            }
            if let Some(p) = s.pressure {
                if p > 0.0 {
                    let _ = write!(out, " pressure='{p:.1} bar'");
                }
            }
            out.push_str(" />\n");
        }

        out.push_str("  </divecomputer>\n</dive>\n");
    }

    out.push_str("</dives>\n</divelog>\n");
    out
}

/// Human-readable gas label (air / nitrox / trimix) from O2/He fractions.
fn gas_label(o2: f64, he: f64) -> String {
    let o2p = (o2 * 100.0).round() as i32;
    let hep = (he * 100.0).round() as i32;
    if hep > 0 {
        format!("Tx{o2p}/{hep}")
    } else if o2p == 21 || o2p == 0 {
        "air".to_string()
    } else {
        format!("Nx{o2p}")
    }
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\'', "&apos;")
}

/// M8 entry point: read a raw dump file, parse the dives, write Subsurface XML,
/// and print a summary for cross-checking against the device display.
pub fn run_parse(
    in_path: &str,
    out_path: &str,
    model_name: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let data = std::fs::read(in_path)?;
    println!("Read {} bytes from {in_path}.", data.len());

    let records = extract_dives(&data);
    if records.is_empty() {
        return Err("no dive records found (marker A5 A5 5A 5A missing)".into());
    }
    println!("Found {} dive(s).\n", records.len());

    let mut dives = Vec::new();
    for (i, rec) in records.iter().enumerate() {
        match parse_dive(rec) {
            Ok(dive) => {
                let (y, mo, d, hh, mm, _ss) = civil(dive.unix_time);
                println!(
                    "  dive {:>3}: {:04}-{:02}-{:02} {:02}:{:02}  {:>3} min  max {:>5.1} m  min {:>4.1} C  {}  {} samples",
                    i + 1,
                    y, mo, d, hh, mm,
                    dive.divetime_s / 60,
                    dive.maxdepth,
                    dive.temp_min,
                    if dive.salinity_gl >= 1025 { "salt " } else { "fresh" },
                    dive.samples.len()
                );
                if !dive.cylinders.is_empty() {
                    let gases: Vec<String> = dive
                        .cylinders
                        .iter()
                        .map(|c| gas_label(c.o2, c.he))
                        .collect();
                    println!("           gas: {}", gases.join(", "));
                }
                dives.push(dive);
            }
            Err(e) => eprintln!("  dive {}: parse error: {e}", i + 1),
        }
    }

    let xml = write_xml(&dives, model_name);
    std::fs::write(out_path, xml)?;
    println!("\nM8: wrote {} dive(s) to {out_path} (Subsurface XML).", dives.len());
    println!("  Import in Subsurface via: File → Import → Import Log Files.");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_bits() {
        assert_eq!(galileo_identify(0b0000_0000), 0); // depth delta
        assert_eq!(galileo_identify(0b0111_1111), 0);
        assert_eq!(galileo_identify(0b1000_0000), 1); // rbt delta
        assert_eq!(galileo_identify(0b1010_0000), 2); // pressure delta
        assert_eq!(galileo_identify(0b1011_0000), 3); // temp delta
        assert_eq!(galileo_identify(0b1100_0000), 4); // time
        assert_eq!(galileo_identify(0b1111_0001), 8); // absolute depth
        assert_eq!(galileo_identify(0b1111_1011), 18); // misc
    }

    #[test]
    fn signextend_works() {
        assert_eq!(signextend(0b0011_1111, 7), 63); // positive 7-bit
        assert_eq!(signextend(0b0000_0001, 7), 1);
        assert_eq!(signextend(0b0111_1111, 7), -1); // 7-bit 0x7F -> -1
        assert_eq!(signextend(0b0100_0000, 7), -64);
        assert_eq!(signextend(0, 0), 0);
    }

    #[test]
    fn civil_epoch() {
        // 2000-01-01 00:00:00 UTC
        assert_eq!(civil(946_684_800), (2000, 1, 1, 0, 0, 0));
        // A known date: 2023-03-06 18:43:26 UTC = 1678128206
        assert_eq!(civil(1_678_128_206), (2023, 3, 6, 18, 43, 26));
    }

    #[test]
    fn extract_finds_records() {
        // Two minimal records back to back: marker + len(=8) + no payload.
        let mut dump = Vec::new();
        for _ in 0..2 {
            dump.extend_from_slice(&MARKER);
            dump.extend_from_slice(&8u32.to_le_bytes());
        }
        let recs = extract_dives(&dump);
        assert_eq!(recs.len(), 2);
        assert!(recs.iter().all(|r| r.len() == 8));
    }
}
