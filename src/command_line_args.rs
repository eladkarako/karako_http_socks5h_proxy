use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

#[derive(Parser, Debug, Clone)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
#[command(
    after_help = concat!("Repository:    ", env!("CARGO_PKG_REPOSITORY"))
)]
pub struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    pub http_port: u16,
    #[arg(short = 'u', long, default_value = "5678")]
    pub upstream_port: u16,
    /// Address to bind the HTTP proxy listener on
    #[arg(short = 'b', long, default_value = "127.0.0.1")]
    pub bind: IpAddr,
    /// Host of the upstream SOCKS5 server
    #[arg(long, default_value = "127.0.0.1")]
    pub upstream_host: IpAddr,
    /// Username for SOCKS5 username/password authentication
    #[arg(long)]
    pub socks5_username: Option<String>,
    /// Password for SOCKS5 username/password authentication
    ///
    /// Prefer --socks5-password-file or KARAKO_SOCKS5_PASSWORD; arguments
    /// are visible to other users through the process list.
    #[arg(long, conflicts_with = "socks5_password_file")]
    pub socks5_password: Option<String>,
    /// File containing the SOCKS5 password (a trailing newline is stripped)
    #[arg(long)]
    pub socks5_password_file: Option<PathBuf>,
    /// Username required from HTTP clients via Proxy-Authorization: Basic
    #[arg(long, requires = "proxy_password")]
    pub proxy_username: Option<String>,
    /// Password required from HTTP clients via Proxy-Authorization: Basic
    #[arg(long, requires = "proxy_username")]
    pub proxy_password: Option<String>,
    /// Allow binding to a non-loopback address (exposes the proxy)
    #[arg(long)]
    pub allow_non_local: bool,
    /// Omit the Via header from forwarded requests and responses
    #[arg(long)]
    pub no_via: bool,
    /// Add X-Forwarded-For with the client address
    #[arg(long)]
    pub x_forwarded_for: bool,
    /// Copy buffer size per direction for CONNECT tunnels, in KiB
    #[arg(long, default_value_t = 32)]
    pub tunnel_buffer_size: u16,
    /// Override SO_RCVBUF for all sockets, in KiB (default: per-role)
    #[arg(long)]
    pub socket_recv_buffer: Option<u32>,
    /// Override SO_SNDBUF for all sockets, in KiB (default: per-role)
    #[arg(long)]
    pub socket_send_buffer: Option<u32>,
    /// Increase log verbosity (use twice to dump SOCKS5 packets)
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Only log errors
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
}

pub fn parse_and_validate() -> Result<Args> {
    let mut args = Args::parse();
    resolve_socks5_password(&mut args)?;
    let listen = SocketAddr::new(args.bind, args.http_port);
    let upstream =
        SocketAddr::new(args.upstream_host, args.upstream_port);
    ensure!(
        listen != upstream,
        "HTTP listener ({}) cannot be the same as the upstream \
         SOCKS5 server ({})",
        listen,
        upstream
    );
    ensure!(
        args.bind.is_loopback() || args.allow_non_local,
        "refusing to bind the proxy to non-loopback address {}; \
         pass --allow-non-local to expose it to the network",
        args.bind
    );
    ensure!(
        (1..=1024).contains(&args.tunnel_buffer_size),
        "--tunnel-buffer-size must be between 1 and 1024 KiB"
    );
    if let Some(size) = args.socket_recv_buffer {
        ensure!(
            (1..=1_048_576).contains(&size),
            "--socket-recv-buffer must be between 1 and 1048576 KiB"
        );
    }
    if let Some(size) = args.socket_send_buffer {
        ensure!(
            (1..=1_048_576).contains(&size),
            "--socket-send-buffer must be between 1 and 1048576 KiB"
        );
    }
    Ok(args)
}

impl Args {
    pub fn tunnel_buffer_bytes(&self) -> usize {
        usize::from(self.tunnel_buffer_size) * 1024
    }

    pub fn socket_recv_buffer_bytes(&self) -> Option<usize> {
        self.socket_recv_buffer
            .map(|kib| kib as usize * 1024)
    }

    pub fn socket_send_buffer_bytes(&self) -> Option<usize> {
        self.socket_send_buffer
            .map(|kib| kib as usize * 1024)
    }
}

/// Resolves the SOCKS5 password from the file, the environment, or the
/// command line, in that order, and validates that a username is paired
/// with a password.
fn resolve_socks5_password(args: &mut Args) -> Result<()> {
    if let Some(path) = args.socks5_password_file.as_deref() {
        let contents =
            std::fs::read_to_string(path).with_context(|| {
                format!(
                    "Failed to read SOCKS5 password file {}",
                    path.display()
                )
            })?;
        args.socks5_password = Some(
            contents
                .trim_end_matches(|c| c == '\r' || c == '\n')
                .to_string(),
        );
    }
    if args.socks5_password.is_none() {
        if let Ok(password) = std::env::var("KARAKO_SOCKS5_PASSWORD") {
            args.socks5_password = Some(password);
        }
    }
    match (
        args.socks5_username.is_some(),
        args.socks5_password.is_some(),
    ) {
        (true, false) => bail!(
            "--socks5-username requires --socks5-password, \
             --socks5-password-file, or KARAKO_SOCKS5_PASSWORD"
        ),
        (false, true) => bail!(
            "a SOCKS5 password was provided without --socks5-username"
        ),
        _ => {}
    }
    Ok(())
}
