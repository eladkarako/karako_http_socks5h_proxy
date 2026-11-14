use anyhow::{ensure, Result};
use clap::Parser;
use std::net::{IpAddr, SocketAddr};

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
    #[arg(long, requires = "socks5_password")]
    pub socks5_username: Option<String>,
    /// Password for SOCKS5 username/password authentication
    #[arg(long, requires = "socks5_username")]
    pub socks5_password: Option<String>,
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
    /// Increase log verbosity (use twice to dump SOCKS5 packets)
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,
    /// Only log errors
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
}

pub fn parse_and_validate() -> Result<Args> {
    let args = Args::parse();
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
    Ok(args)
}
