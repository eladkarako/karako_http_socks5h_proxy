use std::sync::atomic::{AtomicU8, Ordering};

/// Log verbosity, ordered from quietest to noisiest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum Level {
    Error = 0,
    Warn = 1,
    Info = 2,
    Debug = 3,
    Trace = 4,
}

static LEVEL: AtomicU8 = AtomicU8::new(Level::Info as u8);

/// Applies the `--quiet` / `--verbose` command-line flags.
///
/// Without flags the proxy reports one line per request plus
/// connection-level events. `-v` adds handshake, socket and timing
/// details; `-vv` also dumps the raw SOCKS5 packets.
pub fn init(
    quiet: bool,
    verbose: u8,
) {
    let level = if quiet {
        Level::Error
    } else {
        match verbose {
            0 => Level::Info,
            1 => Level::Debug,
            _ => Level::Trace,
        }
    };
    LEVEL.store(level as u8, Ordering::Relaxed);
}

pub fn enabled(level: Level) -> bool {
    (level as u8) <= LEVEL.load(Ordering::Relaxed)
}

/// Formats bytes as uppercase hex, sixteen per line, for protocol
/// tracing.
pub fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out =
        String::with_capacity(bytes.len() * 3 + bytes.len() / 8);
    for (index, byte) in bytes.iter().enumerate() {
        if index > 0 && index % 16 == 0 {
            out.push('\n');
        }
        let _ = write!(out, "{:02X} ", byte);
    }
    out.trim_end().to_string()
}

macro_rules! log_at {
    ($level:expr, $($arg:tt)*) => {{
        if $crate::logging::enabled($level) {
            eprintln!($($arg)*);
        }
    }};
}

macro_rules! log_error {
    ($($arg:tt)*) => {
        log_at!($crate::logging::Level::Error, $($arg)*)
    };
}

macro_rules! log_warn {
    ($($arg:tt)*) => {
        log_at!($crate::logging::Level::Warn, $($arg)*)
    };
}

macro_rules! log_info {
    ($($arg:tt)*) => {
        log_at!($crate::logging::Level::Info, $($arg)*)
    };
}

macro_rules! log_debug {
    ($($arg:tt)*) => {
        log_at!($crate::logging::Level::Debug, $($arg)*)
    };
}

macro_rules! log_trace {
    ($($arg:tt)*) => {
        log_at!($crate::logging::Level::Trace, $($arg)*)
    };
}
