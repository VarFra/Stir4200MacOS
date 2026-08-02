//! Minimal leveled logger with hex dump, written by hand to keep dependencies
//! to just `rusb` (brief §7: "Dipendenze minime").
//!
//! The hex dump of every TX/RX frame is meant to be the primary diagnostic
//! tool for this project, so it lives here from the start (brief §7).

use std::sync::atomic::{AtomicU8, Ordering};

/// Verbosity levels. Higher = more output. Set once from the CLI.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Debug)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

pub fn set_level(level: Level) {
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn level() -> Level {
    match LEVEL.load(Ordering::Relaxed) {
        0 => Level::Error,
        1 => Level::Warn,
        2 => Level::Info,
        3 => Level::Debug,
        _ => Level::Trace,
    }
}

pub fn enabled(level: Level) -> bool {
    level <= self::level()
}

/// Internal: emit a line to stderr if the level is enabled.
pub fn log(level: Level, args: std::fmt::Arguments<'_>) {
    if !enabled(level) {
        return;
    }
    let tag = match level {
        Level::Error => "ERROR",
        Level::Warn => "WARN ",
        Level::Info => "INFO ",
        Level::Debug => "DEBUG",
        Level::Trace => "TRACE",
    };
    eprintln!("[{tag}] {args}");
}

#[macro_export]
macro_rules! error { ($($a:tt)*) => { $crate::logging::log($crate::logging::Level::Error, format_args!($($a)*)) } }
#[macro_export]
macro_rules! warn { ($($a:tt)*) => { $crate::logging::log($crate::logging::Level::Warn, format_args!($($a)*)) } }
#[macro_export]
macro_rules! info { ($($a:tt)*) => { $crate::logging::log($crate::logging::Level::Info, format_args!($($a)*)) } }
#[macro_export]
macro_rules! debug { ($($a:tt)*) => { $crate::logging::log($crate::logging::Level::Debug, format_args!($($a)*)) } }
#[macro_export]
macro_rules! trace { ($($a:tt)*) => { $crate::logging::log($crate::logging::Level::Trace, format_args!($($a)*)) } }

/// Format a byte slice as a classic offset/hex/ASCII dump (16 bytes per line).
/// Returned as a String so callers can log it at whatever level they want.
pub fn hexdump(data: &[u8]) -> String {
    let mut out = String::new();
    for (i, chunk) in data.chunks(16).enumerate() {
        let offset = i * 16;
        let mut hex = String::new();
        let mut ascii = String::new();
        for (j, b) in chunk.iter().enumerate() {
            if j == 8 {
                hex.push(' ');
            }
            hex.push_str(&format!("{b:02x} "));
            ascii.push(if b.is_ascii_graphic() || *b == b' ' {
                *b as char
            } else {
                '.'
            });
        }
        // Pad the hex column so the ASCII column lines up on short final rows.
        let width = 16 * 3 + 1; // 16 bytes * "xx " + one extra space at the mid gap
        out.push_str(&format!("  {offset:04x}  {hex:<width$} |{ascii}|\n"));
    }
    if out.ends_with('\n') {
        out.pop();
    }
    out
}

/// Log a labelled hex dump of a frame at DEBUG level (the TX/RX diagnostic path).
pub fn dump_frame(dir: &str, data: &[u8]) {
    if !enabled(Level::Debug) {
        return;
    }
    log(
        Level::Debug,
        format_args!("{dir} {} byte:\n{}", data.len(), hexdump(data)),
    );
}
