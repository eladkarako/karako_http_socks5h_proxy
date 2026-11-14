//! SOCKS5 client handshake used for every upstream connection.
//!
//! The proxy never accepts SOCKS5; it only speaks the client half of
//! RFC 1928 to reach the upstream server, plus RFC 1929
//! username/password authentication when credentials are configured.
//!
//! A session has three steps: greet with the supported authentication
//! methods, authenticate if the server chose that method, then send a
//! `CONNECT` for the destination and consume the bound-address reply.
//! Host names are sent as-is so that the server resolves them remotely
//! (the `s` in SOCKS5h).
//!
//! [`connect_to_socks5_server`] performs the TCP connect and
//! [`establish_socks5_connection`] drives the protocol exchange, both
//! under [`SOCKS5_HANDSHAKE_TIMEOUT`].

use anyhow::{bail, ensure, Context, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::proxy_error_types::ProxyError;
use crate::tcp_socket_config::{
    configure_tokio_socket, SocketConfiguration,
};

// Protocol constants from RFC 1928 (SOCKS5) and RFC 1929
// (username/password authentication).
const SOCKS5_VERSION: u8 = 5;
const SOCKS5_CMD_CONNECT: u8 = 1;
const SOCKS5_AUTH_METHOD_NONE: u8 = 0;
const SOCKS5_AUTH_METHOD_USER_PASS: u8 = 0x02;
const SOCKS5_AUTH_METHOD_UNACCEPTABLE: u8 = 0xFF;
const SOCKS5_AUTH_VERSION: u8 = 0x01;
const SOCKS5_ATYP_DOMAIN: u8 = 3;
const SOCKS5_ATYP_IPV4: u8 = 1;
const SOCKS5_ATYP_IPV6: u8 = 4;
const MAX_HOSTNAME_LEN: usize = 255;

/// The whole handshake, not just TCP connect, must finish in this window.
const SOCKS5_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// Human-readable forms of the `REP` field, indexed by its value.
const SOCKS5_ERRORS: &[&str] = &[
    "",
    "General SOCKS server failure",
    "Connection not allowed by ruleset",
    "Network unreachable",
    "Host unreachable",
    "Connection refused",
    "TTL expired",
    "Command not supported",
    "Address type not supported",
];

/// Optional RFC 1929 username/password credentials for the upstream
/// SOCKS5 server.
#[derive(Clone, Default)]
pub struct Socks5Auth {
    credentials: Option<(String, String)>,
}

impl Socks5Auth {
    /// Builds credentials from the CLI options. Both values must be
    /// present (the argument parser enforces that) or neither is used.
    pub fn new(
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        match (username, password) {
            (Some(username), Some(password)) => Self {
                credentials: Some((username, password)),
            },
            _ => Self { credentials: None },
        }
    }
}

/// Opens a TCP connection to the SOCKS5 server and applies handshake
/// socket settings. The protocol exchange is not started here.
pub async fn connect_to_socks5_server(
    upstream_addr: std::net::SocketAddr
) -> Result<TcpStream> {
    let started = std::time::Instant::now();
    let addr = upstream_addr;
    let stream = match timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        TcpStream::connect(&addr),
    )
        .await
    {
        Ok(result) => result.with_context(|| {
            format!(
                "Failed to connect to SOCKS5 server at {}",
                addr
            )
        })?,
        Err(_) => {
            return Err(ProxyError::Timeout {
                phase: "connecting to the SOCKS5 server",
                duration: SOCKS5_HANDSHAKE_TIMEOUT,
            }
                .into());
        }
    };
    configure_tokio_socket(
        &stream,
        SocketConfiguration::SOCKS_HANDSHAKE,
    );
    log_debug!(
        "[SOCKS5] Connected to {} in {:?}",
        addr,
        started.elapsed()
    );
    Ok(stream)
}

/// Runs the SOCKS5 greeting, optional authentication and `CONNECT`
/// exchange on an already-connected stream.
///
/// The caller must not use the stream for anything else until this
/// returns `Ok`, because the reply to `CONNECT` marks the point where
/// raw tunnel bytes begin.
pub async fn establish_socks5_connection(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth: &Socks5Auth,
) -> Result<()> {
    let started = std::time::Instant::now();
    match timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        perform_socks5_handshake(stream, host, port, auth),
    )
        .await
    {
        Ok(result) => result?,
        Err(_) => {
            return Err(ProxyError::Timeout {
                phase: "SOCKS5 handshake",
                duration: SOCKS5_HANDSHAKE_TIMEOUT,
            }
                .into());
        }
    }
    log_info!(
        "[SOCKS5] Handshake complete for {}:{} in {:?}",
        host,
        port,
        started.elapsed()
    );
    Ok(())
}

/// Drives the greeting and the CONNECT exchange in order, stopping at
/// the first rejected step.
async fn perform_socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
    auth: &Socks5Auth,
) -> Result<()> {
    send_socks5_greeting(stream, auth).await?;
    let method = receive_socks5_greeting_response(stream).await?;
    match method {
        SOCKS5_AUTH_METHOD_NONE => {}
        SOCKS5_AUTH_METHOD_USER_PASS => {
            perform_user_pass_auth(stream, auth).await?;
        }
        other => bail!(
            "SOCKS5 server selected unsupported authentication \
             method 0x{:02X}",
            other
        ),
    }
    send_socks5_connect_request(stream, host, port).await?;
    receive_socks5_connect_response(stream).await
}

/// Offers the authentication methods this proxy can handle.
async fn send_socks5_greeting(
    stream: &mut TcpStream,
    auth: &Socks5Auth,
) -> Result<()> {
    let methods: &[u8] = if auth.credentials.is_some() {
        // Advertise none first: servers that accept it can skip the
        // RFC 1929 exchange entirely.
        &[SOCKS5_AUTH_METHOD_NONE, SOCKS5_AUTH_METHOD_USER_PASS]
    } else {
        &[SOCKS5_AUTH_METHOD_NONE]
    };
    let mut greeting = Vec::with_capacity(2 + methods.len());
    greeting.push(SOCKS5_VERSION);
    greeting.push(methods.len() as u8);
    greeting.extend_from_slice(methods);
    stream
        .write_all(&greeting)
        .await
        .context("Failed to send SOCKS5 greeting")?;
    stream
        .flush()
        .await
        .context("Failed to flush SOCKS5 greeting")?;
    log_trace!(
        "[SOCKS5] Greeting out ({} bytes):\n{}",
        greeting.len(),
        crate::logging_output_setup::hex_dump(&greeting)
    );
    Ok(())
}

/// Reads the method the server selected and rejects `0xFF`.
async fn receive_socks5_greeting_response(
    stream: &mut TcpStream
) -> Result<u8> {
    let mut resp = [0u8; 2];
    stream
        .read_exact(&mut resp)
        .await
        .context("Failed to read SOCKS5 greeting response")?;
    log_trace!(
        "[SOCKS5] Greeting in ({} bytes):\n{}",
        resp.len(),
        crate::logging_output_setup::hex_dump(&resp)
    );
    ensure!(
        resp[0] == SOCKS5_VERSION,
        "Invalid SOCKS5 version: {} (expected {})",
        resp[0],
        SOCKS5_VERSION
    );
    ensure!(
        resp[1] != SOCKS5_AUTH_METHOD_UNACCEPTABLE,
        "SOCKS5 server rejected all offered authentication methods"
    );
    Ok(resp[1])
}

/// Sends RFC 1929 credentials and checks the one-byte status reply.
async fn perform_user_pass_auth(
    stream: &mut TcpStream,
    auth: &Socks5Auth,
) -> Result<()> {
    let Some((username, password)) = auth.credentials.as_ref()
    else {
        bail!(
            "SOCKS5 server requires username/password \
             authentication, but no credentials were configured"
        );
    };
    ensure!(
        username.len() <= 255,
        "SOCKS5 username is too long: {} bytes (max 255)",
        username.len()
    );
    ensure!(
        password.len() <= 255,
        "SOCKS5 password is too long: {} bytes (max 255)",
        password.len()
    );
    let mut request =
        Vec::with_capacity(3 + username.len() + password.len());
    request.push(SOCKS5_AUTH_VERSION);
    request.push(username.len() as u8);
    request.extend_from_slice(username.as_bytes());
    request.push(password.len() as u8);
    request.extend_from_slice(password.as_bytes());
    stream
        .write_all(&request)
        .await
        .context("Failed to send SOCKS5 username/password auth")?;
    stream
        .flush()
        .await
        .context("Failed to flush SOCKS5 username/password auth")?;
    log_debug!(
        "[SOCKS5] Username/password auth sent (username {} bytes, password {} bytes)",
        username.len(),
        password.len()
    );
    let mut resp = [0u8; 2];
    stream
        .read_exact(&mut resp)
        .await
        .context("Failed to read SOCKS5 auth response")?;
    ensure!(
        resp[0] == SOCKS5_AUTH_VERSION,
        "Invalid SOCKS5 auth version in response: {} (expected {})",
        resp[0],
        SOCKS5_AUTH_VERSION
    );
    ensure!(
        resp[1] == 0,
        "SOCKS5 username/password authentication failed \
         (status 0x{:02X})",
        resp[1]
    );
    log_debug!("[SOCKS5] Username/password authentication accepted");
    Ok(())
}

/// Destination encoded for the CONNECT request.
///
/// IP literals are sent as IP address types; everything else is sent as a
/// domain name so that the SOCKS5 server does the DNS lookup.
enum Socks5Address<'a> {
    Ipv4([u8; 4]),
    Ipv6([u8; 16]),
    Domain(&'a str),
}

/// Classifies a host as IPv4, IPv6 or a domain name.
///
/// Brackets around IPv6 literals are removed first so that the address
/// parses; the brackets matter in URLs but not on the wire.
fn classify_host(host: &str) -> Result<Socks5Address<'_>> {
    let bare = host
        .strip_prefix('[')
        .and_then(|h| h.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = bare.parse::<std::net::Ipv4Addr>() {
        return Ok(Socks5Address::Ipv4(ip.octets()));
    }
    if let Ok(ip) = bare.parse::<std::net::Ipv6Addr>() {
        return Ok(Socks5Address::Ipv6(ip.octets()));
    }
    ensure!(
        !host.is_empty() && host.len() <= MAX_HOSTNAME_LEN,
        "Invalid hostname length: {} bytes (expected 1..={})",
        host.len(),
        MAX_HOSTNAME_LEN
    );
    Ok(Socks5Address::Domain(host))
}

/// Encodes and sends the `CONNECT` request for `host:port`.
async fn send_socks5_connect_request(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    let address = classify_host(host)?;
    let atyp = match address {
        Socks5Address::Ipv4(_) => SOCKS5_ATYP_IPV4,
        Socks5Address::Ipv6(_) => SOCKS5_ATYP_IPV6,
        Socks5Address::Domain(_) => SOCKS5_ATYP_DOMAIN,
    };
    let address_len = match address {
        Socks5Address::Ipv4(_) => 4,
        Socks5Address::Ipv6(_) => 16,
        Socks5Address::Domain(name) => 1 + name.len(),
    };
    let mut connect_req = Vec::with_capacity(4 + address_len + 2);
    connect_req.extend_from_slice(&[
        SOCKS5_VERSION,
        SOCKS5_CMD_CONNECT,
        0,
        atyp,
    ]);
    match &address {
        Socks5Address::Ipv4(octets) => {
            connect_req.extend_from_slice(octets);
        }
        Socks5Address::Ipv6(octets) => {
            connect_req.extend_from_slice(octets);
        }
        Socks5Address::Domain(name) => {
            connect_req.push(name.len() as u8);
            connect_req.extend_from_slice(name.as_bytes());
        }
    }
    connect_req.extend_from_slice(&port.to_be_bytes());
    log_debug!(
        "[SOCKS5] CONNECT {}:{} (atyp=0x{:02X}, {} bytes)",
        host,
        port,
        atyp,
        connect_req.len()
    );
    log_trace!(
        "[SOCKS5] CONNECT out ({} bytes):\n{}",
        connect_req.len(),
        crate::logging_output_setup::hex_dump(&connect_req)
    );
    stream
        .write_all(&connect_req)
        .await
        .context("Failed to send SOCKS5 CONNECT")?;
    stream
        .flush()
        .await
        .context("Failed to flush SOCKS5 CONNECT")
}

/// Validates the `CONNECT` reply and consumes the bound address that
/// follows it.
async fn receive_socks5_connect_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp_header = [0u8; 4];
    stream
        .read_exact(&mut resp_header)
        .await
        .context("Failed to read SOCKS5 CONNECT response")?;
    log_debug!(
        "[SOCKS5] CONNECT response: ver={}, rep={}, rsv={}, atyp={}",
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3]
    );
    log_trace!(
        "[SOCKS5] CONNECT response header ({} bytes):\n{}",
        resp_header.len(),
        crate::logging_output_setup::hex_dump(&resp_header)
    );
    ensure!(
        resp_header[0] == SOCKS5_VERSION,
        "Invalid SOCKS5 version in response: {} (expected {})",
        resp_header[0],
        SOCKS5_VERSION
    );
    ensure!(
        resp_header[2] == 0,
        "Invalid SOCKS5 reserved byte in response: 0x{:02X}",
        resp_header[2]
    );
    if resp_header[1] != 0 {
        let error_msg =
            if (resp_header[1] as usize) < SOCKS5_ERRORS.len() {
                SOCKS5_ERRORS[resp_header[1] as usize].to_string()
            } else {
                format!("Unknown error code {}", resp_header[1])
            };
        bail!("SOCKS5 CONNECT failed: {}", error_msg);
    }
    read_socks5_bind_address(stream, resp_header[3]).await
}

/// Discards the address the server bound on its side.
///
/// The proxy never uses this address; it only has to read the bytes so
/// that the next byte on the stream is the start of tunnel data.
async fn read_socks5_bind_address(
    stream: &mut TcpStream,
    atyp: u8,
) -> Result<()> {
    match atyp {
        SOCKS5_ATYP_IPV4 => {
            let mut addr_port = [0u8; 6];
            stream
                .read_exact(&mut addr_port)
                .await
                .context("Failed to read SOCKS5 IPv4 response")?;
        }
        SOCKS5_ATYP_DOMAIN => {
            let mut len_byte = [0u8; 1];
            stream
                .read_exact(&mut len_byte)
                .await
                .context("Failed to read SOCKS5 domain length")?;
            let domain_len = len_byte[0] as usize;
            ensure!(
                domain_len > 0 && domain_len <= MAX_HOSTNAME_LEN,
                "Invalid SOCKS5 domain length: {}",
                domain_len
            );
            let mut domain_and_port = vec![0u8; domain_len + 2];
            stream
                .read_exact(&mut domain_and_port)
                .await
                .context("Failed to read SOCKS5 domain response")?;
        }
        SOCKS5_ATYP_IPV6 => {
            let mut addr_port = [0u8; 18];
            stream
                .read_exact(&mut addr_port)
                .await
                .context("Failed to read SOCKS5 IPv6 response")?;
        }
        atype => {
            bail!(
                "Unsupported SOCKS5 address type in response: {}",
                atype
            );
        }
    }
    log_trace!(
        "[SOCKS5] Bind address consumed (atyp=0x{:02X})",
        atyp
    );
    Ok(())
}
