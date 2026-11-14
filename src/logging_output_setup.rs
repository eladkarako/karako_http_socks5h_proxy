use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::filter::LevelFilter;

/// Applies the `--quiet` / `--verbose` command-line flags.
///
/// Without flags the proxy reports one line per request plus
/// connection-level events. `-v` adds handshake, socket and timing
/// details; `-vv` also dumps the raw SOCKS5 packets.
///
/// Logging is written through a non-blocking channel so that a slow
/// console or pipe cannot stall request handling. The returned guard
/// must be kept alive by the caller; dropping it flushes pending lines.
pub fn init(
    quiet: bool,
    verbose: u8,
) -> WorkerGuard {
    let level = if quiet {
        LevelFilter::ERROR
    } else {
        match verbose {
            0 => LevelFilter::INFO,
            1 => LevelFilter::DEBUG,
            _ => LevelFilter::TRACE,
        }
    };
    let (writer, guard) =
        tracing_appender::non_blocking(std::io::stderr());
    let _ = tracing_subscriber::fmt()
        .with_writer(writer)
        .with_max_level(level)
        .with_ansi(false)
        .without_time()
        .with_target(false)
        .with_level(false)
        .try_init();
    guard
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

macro_rules! log_error {
    ($($arg:tt)*) => {
        ::tracing::error!($($arg)*)
    };
}

macro_rules! log_warn {
    ($($arg:tt)*) => {
        ::tracing::warn!($($arg)*)
    };
}

macro_rules! log_info {
    ($($arg:tt)*) => {
        ::tracing::info!($($arg)*)
    };
}

macro_rules! log_debug {
    ($($arg:tt)*) => {
        ::tracing::debug!($($arg)*)
    };
}

macro_rules! log_trace {
    ($($arg:tt)*) => {
        ::tracing::trace!($($arg)*)
    };
}
