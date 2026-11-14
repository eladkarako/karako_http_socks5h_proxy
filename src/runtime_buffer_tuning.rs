//! Process-wide buffer tuning chosen at startup.
//!
//! The socket and tunnel layers read these values on every new socket, so
//! they are published once before the listener starts and then treated as
//! immutable. A `OnceLock` keeps that read lock-free.

use std::sync::OnceLock;

/// Startup buffer tuning shared by the socket and tunnel layers.
///
/// This is set once before the listener starts, so plain reads are
/// effectively immutable for the lifetime of the process.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    /// Global `SO_RCVBUF` override in bytes, if the user set one.
    pub socket_recv_buffer: Option<usize>,
    /// Global `SO_SNDBUF` override in bytes, if the user set one.
    pub socket_send_buffer: Option<usize>,
    /// CONNECT copy buffer size in bytes, applied per direction.
    pub tunnel_buffer_size: usize,
}

impl Default for Tuning {
    fn default() -> Self {
        Self {
            socket_recv_buffer: None,
            socket_send_buffer: None,
            tunnel_buffer_size: 32 * 1024,
        }
    }
}

static TUNING: OnceLock<Tuning> = OnceLock::new();

/// Publishes the tuning values. Only the first call has an effect.
pub(crate) fn init(tuning: Tuning) {
    let _ = TUNING.set(tuning);
}

/// Returns the published values, or the defaults if [`init`] has not run.
pub(crate) fn get() -> Tuning {
    TUNING.get().copied().unwrap_or_default()
}
