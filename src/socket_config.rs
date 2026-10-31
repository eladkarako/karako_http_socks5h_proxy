use anyhow::{Context, Result};
use socket2::SockRef;
use std::time::Duration;
use tokio::net::TcpStream;
#[derive(Debug, Clone, Copy)]
pub struct SocketConfiguration {
    pub recv_buffer: usize,
    pub send_buffer: usize,
    pub tcp_cork: bool,
    pub tcp_quickack: bool,
    pub keepalive_idle: Duration,
    pub keepalive_interval: Duration,
    pub keepalive_retries: u32,
    pub name: &'static str,
}
impl SocketConfiguration {
    pub const CLIENT_INBOUND: Self = Self {
        recv_buffer: 2 * 1024 * 1024,
        send_buffer: 2 * 1024 * 1024,
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(45),
        keepalive_interval: Duration::from_secs(15),
        keepalive_retries: 3,
        name: "ClientInbound",
    };
    pub const SOCKS_HANDSHAKE: Self = Self {
        recv_buffer: 256 * 1024,
        send_buffer: 256 * 1024,
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(30),
        keepalive_interval: Duration::from_secs(10),
        keepalive_retries: 3,
        name: "SocksHandshake",
    };
    pub const SOCKS_TUNNEL: Self = Self {
        recv_buffer: 4 * 1024 * 1024,
        send_buffer: 4 * 1024 * 1024,
        tcp_cork: true,
        tcp_quickack: false,
        keepalive_idle: Duration::from_secs(120),
        keepalive_interval: Duration::from_secs(30),
        keepalive_retries: 3,
        name: "SocksTunnel",
    };
    pub const SOCKS_HTTP_REQUEST: Self = Self {
        recv_buffer: 2 * 1024 * 1024,
        send_buffer: 2 * 1024 * 1024,
        tcp_cork: false,
        tcp_quickack: true,
        keepalive_idle: Duration::from_secs(30),
        keepalive_interval: Duration::from_secs(10),
        keepalive_retries: 5,
        name: "SocksHttpRequest",
    };
}
pub fn configure_tokio_socket(
    stream: &TcpStream,
    config: SocketConfiguration,
) -> Result<()> {
    let socket_ref = SockRef::from(stream);
    socket_ref
        .set_tcp_nodelay(true)
        .context("Failed to set TCP_NODELAY")?;
    socket_ref
        .set_linger(Some(Duration::from_secs(5)))
        .context("Failed to set SO_LINGER")?;
    socket_ref.set_recv_buffer_size(config.recv_buffer).context(
        format!(
            "Failed to set SO_RCVBUF to {} bytes",
            config.recv_buffer
        ),
    )?;
    socket_ref.set_send_buffer_size(config.send_buffer).context(
        format!(
            "Failed to set SO_SNDBUF to {} bytes",
            config.send_buffer
        ),
    )?;
    #[cfg(any(target_os = "android", target_os = "linux"))]
    apply_linux_optimizations(&socket_ref, &config)?;
    #[cfg(target_os = "windows")]
    apply_windows_optimizations(&socket_ref, &config)?;
    socket_ref
        .set_keepalive(true)
        .context("Failed to enable TCP keepalive")?;
    socket_ref
        .set_tcp_keepalive(
            &socket2::TcpKeepalive::new()
                .with_time(config.keepalive_idle)
                .with_interval(config.keepalive_interval)
                .with_retries(config.keepalive_retries),
        )
        .context(
            "Failed to override TCP_KEEPALIVE default configuration",
        )?;
    eprintln!(
        "[SOCKET] [{}] Configured: recv={}MB send={}MB | keepalive idle={}s interval={}s retries={}",
        config.name,
        config.recv_buffer / (1024 * 1024),
        config.send_buffer / (1024 * 1024),
        config.keepalive_idle.as_secs(),
        config.keepalive_interval.as_secs(),
        config.keepalive_retries
    );
    Ok(())
}
#[cfg(any(target_os = "android", target_os = "linux"))]
fn apply_linux_optimizations(
    socket_ref: &SockRef,
    config: &SocketConfiguration,
) -> Result<()> {
    if config.tcp_cork {
        socket_ref
            .set_tcp_cork(true)
            .context("Failed to set TCP_CORK")?;
        eprintln!("[SOCKET] [{}] TCP_CORK enabled", config.name);
    }
    socket_ref
        .set_tcp_quickack(config.tcp_quickack)
        .context("Failed to set TCP_QUICKACK")?;
    if config.tcp_quickack {
        eprintln!("[SOCKET] [{}] TCP_QUICKACK enabled", config.name);
    }
    let user_timeout =
        Duration::from_secs(config.keepalive_idle.as_secs() * 2);
    socket_ref
        .set_tcp_user_timeout(Some(user_timeout))
        .context("Failed to set TCP_USER_TIMEOUT")?;
    eprintln!(
        "[SOCKET] [{}] TCP_USER_TIMEOUT={}s",
        config.name,
        user_timeout.as_secs()
    );
    Ok(())
}
#[cfg(target_os = "windows")]
fn apply_windows_optimizations(
    socket_ref: &SockRef,
    config: &SocketConfiguration,
) -> Result<()> {
    if config.tcp_quickack {
        socket_ref
            .set_tcp_ack_frequency(1)
            .context("Failed to set TCP_ACK_FREQUENCY")?;
        eprintln!(
            "[SOCKET] [{}] TCP_ACK_FREQUENCY=1 (immediate ACKs)",
            config.name
        );
    }
    Ok(())
}
