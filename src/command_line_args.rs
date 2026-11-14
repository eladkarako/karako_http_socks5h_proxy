//! Command-line surface for the proxy.
//!
//! `clap` derives `--help` from the doc comments on [`Args`] and its
//! fields: the first line of each comment becomes the short help shown
//! by `-h`, and any following paragraph becomes the long help shown by
//! `--help`. Keep those comments descriptive enough to stand alone.
//!
//! [`parse_and_validate`] is the only entry point the rest of the
//! program uses; it resolves the SOCKS5 password sources and rejects
//! configurations that would fail later anyway.

use anyhow::{bail, ensure, Context, Result};
use clap::Parser;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

/// A forward proxy that carries HTTP traffic over an upstream SOCKS5h
/// server.
///
/// Clients speak HTTP/1.1 or HTTP/2 to this process. Plain HTTP requests
/// are forwarded as requests, while `CONNECT` opens a raw tunnel for TLS
/// and anything else. Destination names are handed to the SOCKS5 server,
/// so DNS resolution happens on the far side of the tunnel.
#[derive(Parser, Debug, Clone)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(
    about = env!("CARGO_PKG_DESCRIPTION"),
    long_about = concat!(
        "HTTP-over-SOCKS5h forward proxy.\n\n",
        "Listens for HTTP/1.1 and HTTP/2 clients and forwards their ",
        "requests through an upstream SOCKS5h server. Host names travel ",
        "to that server unresolved, so remote DNS is used.\n\n",
        "Plain HTTP is forwarded directly; HTTPS and other TLS traffic ",
        "must use CONNECT.\n\n",
        "Example:\n",
        "  karako_http_socks5h_proxy -p 18765 -u 9150\n",
        "  curl -x http://127.0.0.1:18765 http://example.com/"
    )
)]
#[command(
    after_help = concat!("Repository:    ", env!("CARGO_PKG_REPOSITORY"))
)]
pub struct Args {
    /// Port the HTTP proxy listens on for clients
    ///
    /// Point clients at `http://<bind>:<http-port>`. This is the port a
    /// browser or `curl -x` should use, not the SOCKS5 port.
    #[arg(short = 'p', long, default_value = "8765")]
    pub http_port: u16,

    /// Port of the upstream SOCKS5 server
    ///
    /// Usually a local Tor instance (9050, or 9150 for the Tor Browser
    /// bundle). The proxy never speaks SOCKS5 to clients.
    #[arg(short = 'u', long, default_value = "5678")]
    pub upstream_port: u16,

    /// Address to bind the HTTP proxy listener on
    ///
    /// Defaults to loopback. Any non-loopback address requires
    /// `--allow-non-local`, because the listener exposes the upstream
    /// SOCKS5 server to everyone who can reach it.
    #[arg(short = 'b', long, default_value = "127.0.0.1")]
    pub bind: IpAddr,

    /// Address of the upstream SOCKS5 server
    ///
    /// Separate from `--bind` so the proxy can listen on one interface
    /// while reaching the SOCKS5 server over another.
    #[arg(long, default_value = "127.0.0.1")]
    pub upstream_host: IpAddr,

    /// Username for SOCKS5 username/password authentication
    ///
    /// Requires a password from `--socks5-password`,
    /// `--socks5-password-file`, or `KARAKO_SOCKS5_PASSWORD`. When this
    /// is omitted, only the no-authentication method is offered.
    #[arg(long)]
    pub socks5_username: Option<String>,

    /// Password for SOCKS5 username/password authentication
    ///
    /// Prefer `--socks5-password-file` or `KARAKO_SOCKS5_PASSWORD`:
    /// command-line arguments are visible to other users through the
    /// process list.
    #[arg(long, conflicts_with = "socks5_password_file")]
    pub socks5_password: Option<String>,

    /// File containing the SOCKS5 password
    ///
    /// One trailing newline (LF or CRLF) is stripped. Takes precedence
    /// over `KARAKO_SOCKS5_PASSWORD`, which takes precedence over
    /// `--socks5-password`.
    #[arg(long, value_name = "PATH")]
    pub socks5_password_file: Option<PathBuf>,

    /// Username HTTP clients must send via Proxy-Authorization
    ///
    /// Turns the listener into an authenticated proxy. Requires
    /// `--proxy-password`. Clients then use
    /// `Proxy-Authorization: Basic <base64(user:pass)>`, which `curl`
    /// does automatically for `-x http://user:pass@host:port`.
    #[arg(long, requires = "proxy_password")]
    pub proxy_username: Option<String>,

    /// Password HTTP clients must send via Proxy-Authorization
    ///
    /// Requests without a matching header receive `407 Proxy
    /// Authentication Required` with a Basic challenge.
    #[arg(long, requires = "proxy_username")]
    pub proxy_password: Option<String>,

    /// Allow binding to a non-loopback address
    ///
    /// Without this flag the proxy refuses to start on a public
    /// interface. Anyone who can reach the listener can use the upstream
    /// SOCKS5 server, so pair this with client authentication.
    #[arg(long)]
    pub allow_non_local: bool,

    /// Omit the Via header from forwarded messages
    ///
    /// `Via` identifies this proxy in requests and responses (RFC 9110).
    /// Use this when the proxy should stay invisible to the origin
    /// server.
    #[arg(long)]
    pub no_via: bool,

    /// Add X-Forwarded-For with the client address
    ///
    /// A client-supplied `X-Forwarded-For` is discarded and replaced, so
    /// the header cannot be used to spoof the original address.
    #[arg(long)]
    pub x_forwarded_for: bool,

    /// Copy buffer size per direction for CONNECT tunnels, in KiB
    ///
    /// Each direction of a tunnel gets its own buffer of this size. The
    /// default suits most links; raise it for high-bandwidth tunnels on
    /// high-latency paths.
    #[arg(long, value_name = "KIB", default_value_t = 32)]
    pub tunnel_buffer_size: u16,

    /// Override SO_RCVBUF for all sockets, in KiB
    ///
    /// Applies to the listener, SOCKS5 handshakes, upstream HTTP
    /// connections, and tunnels. When omitted, each role keeps its own
    /// default (64 KiB for handshakes up to 512 KiB for tunnels).
    #[arg(long, value_name = "KIB")]
    pub socket_recv_buffer: Option<u32>,

    /// Override SO_SNDBUF for all sockets, in KiB
    ///
    /// Same coverage as `--socket-recv-buffer`. The kernel may clamp the
    /// requested value; the effective size is logged with `-v`.
    #[arg(long, value_name = "KIB")]
    pub socket_send_buffer: Option<u32>,

    /// Increase log verbosity
    ///
    /// No flag logs one line per request. `-v` adds handshake, socket and
    /// timing details. `-vv` also dumps SOCKS5 packets as hex.
    #[arg(short = 'v', long, action = clap::ArgAction::Count)]
    pub verbose: u8,

    /// Only log errors
    ///
    /// Mutually exclusive with `--verbose`. Useful when the proxy runs in
    /// the background and only failures are interesting.
    #[arg(short = 'q', long, conflicts_with = "verbose")]
    pub quiet: bool,
}

/// Parses the command line, resolves the SOCKS5 password, and checks the
/// resulting configuration.
///
/// Validation happens here rather than at first use so that a bad
/// combination fails before the listener is bound.
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

/// Converts the KiB command-line values into bytes for the rest of the
/// program.
impl Args {
    /// Tunnel copy buffer in bytes, used per direction.
    pub fn tunnel_buffer_bytes(&self) -> usize {
        usize::from(self.tunnel_buffer_size) * 1024
    }

    /// Global receive-buffer override in bytes, if one was requested.
    pub fn socket_recv_buffer_bytes(&self) -> Option<usize> {
        self.socket_recv_buffer
            .map(|kib| kib as usize * 1024)
    }

    /// Global send-buffer override in bytes, if one was requested.
    pub fn socket_send_buffer_bytes(&self) -> Option<usize> {
        self.socket_send_buffer
            .map(|kib| kib as usize * 1024)
    }
}

/// Resolves the SOCKS5 password from the file, the environment, or the
/// command line, in that order, and validates that a username is paired
/// with a password.
///
/// Keeping this separate from clap's `requires` lets one password source
/// satisfy `--socks5-username` regardless of which mechanism supplied it.
fn resolve_socks5_password(args: &mut Args) -> Result<()> {
    if let Some(path) = args.socks5_password_file.as_deref() {
        let contents =
            std::fs::read_to_string(path).with_context(|| {
                format!(
                    "Failed to read SOCKS5 password file {}",
                    path.display()
                )
            })?;
        // Editors on Windows often leave a CRLF behind; strip either
        // ending so the password matches what the user typed.
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
