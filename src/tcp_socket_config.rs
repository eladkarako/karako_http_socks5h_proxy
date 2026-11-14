//! Per-role socket settings.
//!
//! Each socket the proxy opens has a different traffic pattern, so the
//! buffer sizes and keepalive timings live in named [`SocketConfiguration`]
//! constants instead of one global default. Everything here is
//! best-effort: an unsupported option is logged and the connection
//! continues.
//!
//! Command-line overrides from [`crate::runtime_buffer_tuning`] replace
//! the receive/send sizes for every role at once.

use socket2::SockRef;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

/// Tuning for one kind of socket.
#[derive(Debug, Clone, Copy)]
pub struct SocketConfiguration {
    /// Requested `SO_RCVBUF`, unless a global override is set.
    pub recv_buffer: usize,
    /// Requested `SO_SNDBUF`, unless a global override is set.
    pub send_buffer: usize,
    /// Whether to enable `TCP_CORK` (Linux/Android only).
    #[cfg(any(target_os = "android", target_os = "linux"))]
    pub tcp_cork: bool,
    /// Whether to ask for quick ACKs where the platform supports it.
    pub tcp_quickack: bool,
    /// Idle time before the first keepalive probe.
    pub keepalive_idle: Duration,
    /// Gap between keepalive probes.
    pub keepalive_interval: Duration,
    /// Probes before the connection is considered dead.
    pub keepalive_retries: u32,
    /// Short name used in socket log lines.
    pub name: &'static str,
}

impl SocketConfiguration {
    /// Sockets accepted from clients: mostly request/response headers.
    pub const CLIENT_INBOUND: Self = Self {
        recv_buffer: 256 * 1024,
        send_buffer: 256 * 1024,
        #[cfg(any(target_os = "android", target_os = "linux"))]
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(45),
        keepalive_interval: Duration::from_secs(15),
        keepalive_retries: 3,
        name: "ClientInbound",
    };

    /// Sockets to the SOCKS5 server while the handshake is running.
    pub const SOCKS_HANDSHAKE: Self = Self {
        recv_buffer: 64 * 1024,
        send_buffer: 64 * 1024,
        #[cfg(any(target_os = "android", target_os = "linux"))]
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(30),
        keepalive_interval: Duration::from_secs(10),
        keepalive_retries: 3,
        name: "SocksHandshake",
    };

    /// Sockets carrying tunneled bytes after a successful `CONNECT`.
    ///
    /// These see sustained bidirectional traffic, so they get the largest
    /// buffers and the longest keepalive idle time.
    pub const SOCKS_TUNNEL: Self = Self {
        recv_buffer: 512 * 1024,
        send_buffer: 512 * 1024,
        #[cfg(any(target_os = "android", target_os = "linux"))]
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(120),
        keepalive_interval: Duration::from_secs(30),
        keepalive_retries: 3,
        name: "SocksTunnel",
    };

    /// Pooled sockets used for forwarded HTTP requests.
    pub const SOCKS_HTTP_REQUEST: Self = Self {
        recv_buffer: 256 * 1024,
        send_buffer: 256 * 1024,
        #[cfg(any(target_os = "android", target_os = "linux"))]
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(30),
        keepalive_interval: Duration::from_secs(10),
        keepalive_retries: 5,
        name: "SocksHttpRequest",
    };
}

/// Applies receive/send buffers to the listening socket so that accepted
/// sockets negotiate window scaling with useful limits.
pub fn configure_listener_socket(listener: &TcpListener) {
    let socket_ref = SockRef::from(listener);
    let config = SocketConfiguration::CLIENT_INBOUND;
    let (recv, send) = effective_buffers(&config);
    report(
        config.name,
        "SO_RCVBUF",
        socket_ref.set_recv_buffer_size(recv),
    );
    report(
        config.name,
        "SO_SNDBUF",
        socket_ref.set_send_buffer_size(send),
    );
    log_debug!(
        "[SOCKET] [{}] Listener buffers: recv={}KB send={}KB",
        config.name,
        recv / 1024,
        send / 1024
    );
}

/// Returns the configured buffer sizes, falling back to the per-role
/// defaults when the user did not override them on the command line.
fn effective_buffers(config: &SocketConfiguration) -> (usize, usize) {
    let tuning = crate::runtime_buffer_tuning::get();
    (
        tuning
            .socket_recv_buffer
            .unwrap_or(config.recv_buffer),
        tuning
            .socket_send_buffer
            .unwrap_or(config.send_buffer),
    )
}

/// Applies best-effort socket tuning. None of these options is required
/// for correctness, so failures are logged instead of failing the
/// connection.
pub fn configure_tokio_socket(
    stream: &TcpStream,
    config: SocketConfiguration,
) {
    let socket_ref = SockRef::from(stream);
    let (recv, send) = effective_buffers(&config);
    report(
        config.name,
        "TCP_NODELAY",
        socket_ref.set_tcp_nodelay(true),
    );
    report(
        config.name,
        "SO_RCVBUF",
        socket_ref.set_recv_buffer_size(recv),
    );
    report(
        config.name,
        "SO_SNDBUF",
        socket_ref.set_send_buffer_size(send),
    );

    #[cfg(any(target_os = "android", target_os = "linux"))]
    apply_linux_optimizations(&socket_ref, &config);

    #[cfg(target_os = "windows")]
    apply_windows_optimizations(&socket_ref, &config);

    report(
        config.name,
        "SO_KEEPALIVE",
        socket_ref.set_keepalive(true),
    );
    report(
        config.name,
        "TCP_KEEPALIVE",
        socket_ref.set_tcp_keepalive(
            &socket2::TcpKeepalive::new()
                .with_time(config.keepalive_idle)
                .with_interval(config.keepalive_interval)
                .with_retries(config.keepalive_retries),
        ),
    );
    log_debug!(
        "[SOCKET] [{}] recv={}KB send={}KB | keepalive idle={}s interval={}s retries={}",
        config.name,
        recv / 1024,
        send / 1024,
        config.keepalive_idle.as_secs(),
        config.keepalive_interval.as_secs(),
        config.keepalive_retries
    );
}

/// Logs a failed socket option instead of failing the connection.
///
/// None of these options affects correctness, and some are unavailable
/// on older kernels or non-default network stacks.
fn report<T>(
    socket_name: &str,
    option: &str,
    result: std::io::Result<T>,
) {
    if let Err(error) = result {
        log_warn!(
            "[SOCKET] [{}] Failed to set {}: {}",
            socket_name,
            option,
            error
        );
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
/// Applies the Linux-only options: optional corking, quick ACKs, and a
/// user timeout derived from the keepalive idle time.
fn apply_linux_optimizations(
    socket_ref: &SockRef,
    config: &SocketConfiguration,
) {
    if config.tcp_cork {
        report(
            config.name,
            "TCP_CORK",
            socket_ref.set_tcp_cork(true),
        );
    }
    report(
        config.name,
        "TCP_QUICKACK",
        socket_ref.set_tcp_quickack(config.tcp_quickack),
    );
    let user_timeout =
        Duration::from_secs(config.keepalive_idle.as_secs() * 2);
    report(
        config.name,
        "TCP_USER_TIMEOUT",
        socket_ref.set_tcp_user_timeout(Some(user_timeout)),
    );
}

#[cfg(target_os = "windows")]
/// Asks Windows to ACK every segment; the closest analogue to quick ACK.
fn apply_windows_optimizations(
    socket_ref: &SockRef,
    config: &SocketConfiguration,
) {
    if config.tcp_quickack {
        report(
            config.name,
            "TCP_ACK_FREQUENCY",
            socket_ref.set_tcp_ack_frequency(1),
        );
    }
}
