//! stir4200 — userspace downloader for Uwatec Galileo dive computers over a
//! SigmaTel STIr4200 IrDA/USB dongle on macOS (Apple Silicon).
//!
//! Milestone M1: USB enumeration. Run with `-v`/`-vv` for more logging.

#[macro_use]
pub mod logging;
pub mod sir;
pub mod usb;

use logging::Level;

const USAGE: &str = "\
stir4200 — download Uwatec Galileo dives via a SigmaTel STIr4200 IrDA dongle

USAGE:
    stir4200 [OPTIONS] [enumerate]

COMMANDS:
    enumerate            (default) M1: open the dongle, claim it, print the
                         descriptor tree and endpoints.

OPTIONS:
    -v, --verbose        increase verbosity (repeatable: -v, -vv, -vvv)
    -d, --device VID:PID override the USB id (default 066f:4200, hex)
    -h, --help           show this help
";

struct Args {
    level: Level,
    vid: u16,
    pid: u16,
}

fn parse_args() -> Result<Args, String> {
    let mut level = Level::Info;
    let mut vid = usb::STIR_VID;
    let mut pid = usb::STIR_PID;
    let mut verbosity: u8 = 0;

    let mut it = std::env::args().skip(1);
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "-h" | "--help" => {
                print!("{USAGE}");
                std::process::exit(0);
            }
            "-v" | "--verbose" => verbosity += 1,
            "-vv" => verbosity += 2,
            "-vvv" => verbosity += 3,
            "-d" | "--device" => {
                let spec = it
                    .next()
                    .ok_or_else(|| "--device requires a VID:PID argument".to_string())?;
                let (v, p) = parse_vid_pid(&spec)?;
                vid = v;
                pid = p;
            }
            "enumerate" => {} // the only (default) command for now
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    level = match verbosity {
        0 => level,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    Ok(Args { level, vid, pid })
}

fn parse_vid_pid(spec: &str) -> Result<(u16, u16), String> {
    let (v, p) = spec
        .split_once(':')
        .ok_or_else(|| format!("expected VID:PID, got '{spec}'"))?;
    let vid = u16::from_str_radix(v.trim_start_matches("0x"), 16)
        .map_err(|_| format!("bad VID '{v}'"))?;
    let pid = u16::from_str_radix(p.trim_start_matches("0x"), 16)
        .map_err(|_| format!("bad PID '{p}'"))?;
    Ok((vid, pid))
}

fn main() {
    let args = match parse_args() {
        Ok(a) => a,
        Err(e) => {
            eprintln!("error: {e}\n\n{USAGE}");
            std::process::exit(2);
        }
    };
    logging::set_level(args.level);

    if let Err(e) = usb::run_enumeration(args.vid, args.pid) {
        error!("{e}");
        std::process::exit(1);
    }
}
