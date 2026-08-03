//! stir4200 — userspace downloader for Uwatec Galileo dive computers over a
//! SigmaTel STIr4200 IrDA/USB dongle on macOS (Apple Silicon).
//!
//! Milestone M1: USB enumeration. Run with `-v`/`-vv` for more logging.

#[macro_use]
pub mod logging;
pub mod chip;
pub mod irlap;
pub mod sir;
pub mod usb;

use logging::Level;

const USAGE: &str = "\
stir4200 — download Uwatec Galileo dives via a SigmaTel STIr4200 IrDA dongle

USAGE:
    stir4200 [OPTIONS] [COMMAND]

COMMANDS:
    enumerate            (default) M1: open the dongle, claim it, print the
                         descriptor tree and endpoints.
    init                 M2: reset the chip, set the baud rate, and verify the
                         registers by reading them back.
    tx                   M3: transmit wrapped SIR test frames on bulk OUT so the
                         IR LED can be observed (smartphone camera).
    rx                   M4: poll bulk IN, de-wrap the SIR stream, and report
                         received bytes and frames.
    discover             M5: send IrLAP XID discovery and report responding
                         devices (address + nickname).
    connect              M6: establish an IrLAP connection (SNRM/UA) and hold it
                         with RR keepalives.

OPTIONS:
    -v, --verbose        increase verbosity (repeatable: -v, -vv, -vvv)
    -d, --device VID:PID override the USB id (default 066f:4200, hex)
    -s, --speed BAUD     baud rate for `init`/`tx`/`rx`/`discover` (default 9600)
    -c, --count N        number of frames for `tx` (default 100)
    -t, --seconds N      listen duration for `rx`, or hold time for `connect`
                         (default 20 for `rx`, 30 for `connect`)
        --slots N        discovery slots: 1/6/8/16 (default 1)
        --retries N      discovery attempts (default 3)
    -h, --help           show this help
";

#[derive(PartialEq)]
enum Command {
    Enumerate,
    Init,
    Tx,
    Rx,
    Discover,
    Connect,
}

struct Args {
    level: Level,
    vid: u16,
    pid: u16,
    speed: u32,
    count: u32,
    seconds: Option<u64>,
    slots: u8,
    retries: u32,
    command: Command,
}

fn parse_args() -> Result<Args, String> {
    let mut level = Level::Info;
    let mut vid = usb::STIR_VID;
    let mut pid = usb::STIR_PID;
    let mut speed: u32 = 9600;
    let mut count: u32 = 100;
    let mut seconds: Option<u64> = None;
    let mut slots: u8 = 1;
    let mut retries: u32 = 3;
    let mut command = Command::Enumerate;
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
            "-s" | "--speed" => {
                let s = it
                    .next()
                    .ok_or_else(|| "--speed requires a BAUD argument".to_string())?;
                speed = s.parse().map_err(|_| format!("bad speed '{s}'"))?;
            }
            "-c" | "--count" => {
                let c = it
                    .next()
                    .ok_or_else(|| "--count requires a number".to_string())?;
                count = c.parse().map_err(|_| format!("bad count '{c}'"))?;
            }
            "-t" | "--seconds" => {
                let t = it
                    .next()
                    .ok_or_else(|| "--seconds requires a number".to_string())?;
                seconds = Some(t.parse().map_err(|_| format!("bad seconds '{t}'"))?);
            }
            "--slots" => {
                let s = it
                    .next()
                    .ok_or_else(|| "--slots requires a number".to_string())?;
                slots = s.parse().map_err(|_| format!("bad slots '{s}'"))?;
            }
            "--retries" => {
                let r = it
                    .next()
                    .ok_or_else(|| "--retries requires a number".to_string())?;
                retries = r.parse().map_err(|_| format!("bad retries '{r}'"))?;
            }
            "enumerate" => command = Command::Enumerate,
            "init" => command = Command::Init,
            "tx" => command = Command::Tx,
            "rx" => command = Command::Rx,
            "discover" => command = Command::Discover,
            "connect" => command = Command::Connect,
            other => return Err(format!("unknown argument: {other}")),
        }
    }

    level = match verbosity {
        0 => level,
        1 => Level::Debug,
        _ => Level::Trace,
    };

    Ok(Args {
        level,
        vid,
        pid,
        speed,
        count,
        seconds,
        slots,
        retries,
        command,
    })
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

    let result = match args.command {
        Command::Enumerate => usb::run_enumeration(args.vid, args.pid),
        Command::Init => chip::run_init(args.vid, args.pid, args.speed),
        Command::Tx => chip::run_tx(args.vid, args.pid, args.speed, args.count),
        Command::Rx => chip::run_rx(args.vid, args.pid, args.speed, args.seconds.unwrap_or(20)),
        Command::Discover => {
            irlap::run_discovery(args.vid, args.pid, args.speed, args.slots, args.retries)
        }
        Command::Connect => {
            irlap::run_connect(args.vid, args.pid, args.speed, args.seconds.unwrap_or(30))
        }
    };
    if let Err(e) = result {
        error!("{e}");
        std::process::exit(1);
    }
}
