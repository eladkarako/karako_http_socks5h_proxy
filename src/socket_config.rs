use socket2::SockRef;
use std::time::Duration;
use tokio::net::{TcpListener, TcpStream};

#[derive(Debug, Clone, Copy)]
pub struct SocketConfiguration {
    pub recv_buffer: usize,
    pub send_buffer: usize,
    #[cfg(any(target_os = "android", target_os = "linux"))]
    pub tcp_cork: bool,
    pub tcp_quickack: bool,
    pub keepalive_idle: Duration,
    pub keepalive_interval: Duration,
    pub keepalive_retries: u32,
    pub name: &'static str,
}

impl SocketConfiguration {
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
    report(
        config.name,
        "SO_RCVBUF",
        socket_ref.set_recv_buffer_size(config.recv_buffer),
    );
    report(
        config.name,
        "SO_SNDBUF",
        socket_ref.set_send_buffer_size(config.send_buffer),
    );
}

/// Applies best-effort socket tuning. None of these options is required
/// for correctness, so failures are logged instead of failing the
/// connection.
pub fn configure_tokio_socket(
    stream: &TcpStream,
    config: SocketConfiguration,
) {
    let socket_ref = SockRef::from(stream);
    report(
        config.name,
        "TCP_NODELAY",
        socket_ref.set_tcp_nodelay(true),
    );
    report(
        config.name,
        "SO_RCVBUF",
        socket_ref.set_recv_buffer_size(config.recv_buffer),
    );
    report(
        config.name,
        "SO_SNDBUF",
        socket_ref.set_send_buffer_size(config.send_buffer),
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
    eprintln!(
        "[SOCKET] [{}] Configured: recv={}KB send={}KB | keepalive idle={}s interval={}s retries={}",
        config.name,
        config.recv_buffer / 1024,
        config.send_buffer / 1024,
        config.keepalive_idle.as_secs(),
        config.keepalive_interval.as_secs(),
        config.keepalive_retries
    );
}

fn report<T>(
    socket_name: &str,
    option: &str,
    result: std::io::Result<T>,
) {
    if let Err(error) = result {
        eprintln!(
            "[SOCKET] [{}] Failed to set {}: {}",
            socket_name, option, error
        );
    }
}

#[cfg(any(target_os = "android", target_os = "linux"))]
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
