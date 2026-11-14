use std::sync::OnceLock;

/// Startup buffer tuning shared by the socket and tunnel layers.
///
/// This is set once before the listener starts, so plain reads are
/// effectively immutable for the lifetime of the process.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tuning {
    pub socket_recv_buffer: Option<usize>,
    pub socket_send_buffer: Option<usize>,
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

pub(crate) fn init(tuning: Tuning) {
    let _ = TUNING.set(tuning);
}

pub(crate) fn get() -> Tuning {
    TUNING.get().copied().unwrap_or_default()
}
