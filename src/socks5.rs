use anyhow::{bail, ensure, Context, Result};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};

const SOCKS5_VERSION: u8 = 5;
const SOCKS5_CMD_CONNECT: u8 = 1;
const SOCKS5_AUTH_METHOD_NONE: u8 = 0;
const SOCKS5_AUTH_METHOD_UNACCEPTABLE: u8 = 0xFF;
const SOCKS5_ATYP_DOMAIN: u8 = 3;
const SOCKS5_ATYP_IPV4: u8 = 1;
const SOCKS5_ATYP_IPV6: u8 = 4;
const MAX_HOSTNAME_LEN: usize = 255;
const SOCKS5_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
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

pub async fn connect_to_socks5_server(
    upstream_port: u16
) -> Result<TcpStream> {
    let addr = format!("127.0.0.1:{}", upstream_port);
    let stream =
        timeout(SOCKS5_HANDSHAKE_TIMEOUT, TcpStream::connect(&addr))
            .await
            .with_context(|| {
                format!(
                    "Timed out connecting to SOCKS5 server at {} \
                     after {:?}",
                    addr, SOCKS5_HANDSHAKE_TIMEOUT
                )
            })?
            .with_context(|| {
                format!(
                    "Failed to connect to SOCKS5 server at {}",
                    addr
                )
            })?;
    configure_tokio_socket(
        &stream,
        SocketConfiguration::SOCKS_HANDSHAKE,
    )?;
    eprintln!("[SOCKS5] Connected to {}", addr);
    Ok(stream)
}

pub async fn establish_socks5_connection(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    eprintln!("[SOCKS5] Starting handshake for {}:{}", host, port);
    timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        perform_socks5_handshake(stream, host, port),
    )
    .await
    .with_context(|| {
        format!(
            "SOCKS5 handshake timed out after {:?} for {}:{}",
            SOCKS5_HANDSHAKE_TIMEOUT, host, port
        )
    })??;
    eprintln!("[SOCKS5] Handshake complete for {}:{}", host, port);
    Ok(())
}

async fn perform_socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    send_socks5_greeting(stream).await?;
    receive_socks5_greeting_response(stream).await?;
    send_socks5_connect_request(stream, host, port).await?;
    receive_socks5_connect_response(stream).await
}

async fn send_socks5_greeting(stream: &mut TcpStream) -> Result<()> {
    let greeting = [SOCKS5_VERSION, 1, SOCKS5_AUTH_METHOD_NONE];
    stream
        .write_all(&greeting)
        .await
        .context("Failed to send SOCKS5 greeting")?;
    stream
        .flush()
        .await
        .context("Failed to flush SOCKS5 greeting")?;
    eprintln!("[SOCKS5] Sent greeting: {:?}", greeting);
    Ok(())
}

async fn receive_socks5_greeting_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp = [0u8; 2];
    stream
        .read_exact(&mut resp)
        .await
        .context("Failed to read SOCKS5 greeting response")?;
    eprintln!(
        "[SOCKS5] Greeting response: {:?} (ver={}, method={})",
        resp, resp[0], resp[1]
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
    ensure!(
        resp[1] == SOCKS5_AUTH_METHOD_NONE,
        "SOCKS5 server selected unsupported authentication method \
         0x{:02X} (only no-authentication is supported)",
        resp[1]
    );
    Ok(())
}

enum Socks5Address<'a> {
    Ipv4([u8; 4]),
    Ipv6([u8; 16]),
    Domain(&'a str),
}

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
    let mut connect_req =
        Vec::with_capacity(4 + address_len + 2);
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
    eprintln!(
        "[SOCKS5] CONNECT {}:{} (atyp=0x{:02X}, {} bytes)",
        host,
        port,
        atyp,
        connect_req.len()
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

async fn receive_socks5_connect_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp_header = [0u8; 4];
    stream
        .read_exact(&mut resp_header)
        .await
        .context("Failed to read SOCKS5 CONNECT response")?;
    eprintln!(
        "[SOCKS5] CONNECT response: ver={}, rep={}, rsv={}, atyp={}",
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3]
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
    Ok(())
}
