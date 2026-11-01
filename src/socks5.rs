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
const SOCKS5_ATYP_DOMAIN: u8 = 3;
const SOCKS5_ATYP_IPV4: u8 = 1;
const SOCKS5_ATYP_IPV6: u8 = 4;
const MAX_HOSTNAME_LEN: usize = 255;
const SOCKS5_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
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
    let stream = timeout(SOCKS5_HANDSHAKE_TIMEOUT, TcpStream::connect(&addr))
        .await
        .with_context(|| {
            format!(
                "Failed (or {:?} timeout) to connect to SOCKS5 server at {}",
                SOCKS5_HANDSHAKE_TIMEOUT, addr
            )
        })?
        .context("SOCKS5 connection failed")?;
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
    send_socks5_greeting(stream).await?;
    receive_socks5_greeting_response(stream).await?;
    send_socks5_connect_request(stream, host, port).await?;
    receive_socks5_connect_response(stream).await?;
    eprintln!("[SOCKS5] Handshake complete for {}:{}", host, port);
    Ok(())
}

async fn send_socks5_greeting(stream: &mut TcpStream) -> Result<()> {
    let greeting = [SOCKS5_VERSION, 1, 0];
    timeout(SOCKS5_HANDSHAKE_TIMEOUT, stream.write_all(&greeting))
        .await
        .context("SOCKS5 greeting write timeout")?
        .context("Failed to send SOCKS5 greeting")?;
    eprintln!("[SOCKS5] Sent greeting: {:?}", greeting);
    Ok(())
}

async fn receive_socks5_greeting_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp = [0u8; 2];
    timeout(SOCKS5_HANDSHAKE_TIMEOUT, stream.read_exact(&mut resp))
        .await
        .context("SOCKS5 greeting read timeout")?
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
    ensure!(resp[1] != 0xff, "SOCKS5 server rejected all methods");
    Ok(())
}

async fn send_socks5_connect_request(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    eprintln!("[SOCKS5] Domain bytes: {:?}", host.as_bytes());
    ensure!(
        host.len() <= MAX_HOSTNAME_LEN,
        "Hostname too long: {} bytes",
        host.len()
    );
    let mut connect_req = vec![0u8; 5 + host.len() + 2];
    connect_req[0] = SOCKS5_VERSION;
    connect_req[1] = SOCKS5_CMD_CONNECT;
    connect_req[2] = 0;
    connect_req[3] = SOCKS5_ATYP_DOMAIN;
    connect_req[4] = host.len() as u8;
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes());
    connect_req[5 + host.len()] = (port >> 8) as u8;
    connect_req[6 + host.len()] = (port & 0xff) as u8;
    eprintln!("[SOCKS5] Host: '{}' (len={})", host, host.len());
    eprintln!(
        "[SOCKS5] Port: {} (0x{:04X}) -> bytes: [0x{:02X}, 0x{:02X}]",
        port,
        port,
        (port >> 8) as u8,
        (port & 0xff) as u8
    );
    eprintln!(
        "[SOCKS5] Full CONNECT request ({} bytes):",
        connect_req.len()
    );
    let mut hex_str = String::new();
    for (i, byte) in connect_req.iter().enumerate() {
        hex_str.push_str(&format!("0x{:02X} ", byte));
        if (i + 1) % 8 == 0 {
            eprintln!("  [{}..{}] {}", i - 7, i, hex_str);
            hex_str.clear();
        }
    }
    if !hex_str.is_empty() {
        eprintln!("  [rest] {}", hex_str);
    }
    eprintln!("[SOCKS5] [0] VER=0x{:02X}", connect_req[0]);
    eprintln!("[SOCKS5] [1] CMD=0x{:02X}", connect_req[1]);
    eprintln!("[SOCKS5] [2] RSV=0x{:02X}", connect_req[2]);
    eprintln!("[SOCKS5] [3] ATYP=0x{:02X}", connect_req[3]);
    eprintln!(
        "[SOCKS5] [4] DLEN=0x{:02X} ({})",
        connect_req[4], connect_req[4]
    );
    eprintln!("[SOCKS5] [5..{}] DNAME={}", 4 + host.len(), host);
    eprintln!(
        "[SOCKS5] [{}..{}] PORT=[0x{:02X}, 0x{:02X}]",
        5 + host.len(),
        5 + host.len() + 1,
        connect_req[5 + host.len()],
        connect_req[5 + host.len() + 1]
    );
    eprintln!(
        "[SOCKS5] Sending CONNECT request ({} bytes) for {}:{}",
        connect_req.len(),
        host,
        port
    );
    timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        stream.write_all(&connect_req),
    )
        .await
        .context("SOCKS5 CONNECT write timeout")?
        .context("Failed to send SOCKS5 CONNECT")?;
    stream
        .flush()
        .await
        .context("Failed to flush SOCKS5 CONNECT")?;
    eprintln!("[SOCKS5] CONNECT request sent successfully");
    Ok(())
}

async fn receive_socks5_connect_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp_header = [0u8; 4];
    timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        stream.read_exact(&mut resp_header),
    )
        .await
        .context("SOCKS5 CONNECT response read timeout")?
        .context("Failed to read SOCKS5 CONNECT response")?;
    eprintln!(
        "[SOCKS5] Raw response bytes: [0x{:02X}, 0x{:02X}, 0x{:02X}, 0x{:02X}]",
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3]
    );
    eprintln!(
        "[SOCKS5] CONNECT response: ver={}, rep={}, rsv={}, atyp={}",
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3]
    );
    ensure!(
        resp_header[0] == SOCKS5_VERSION,
        "Invalid SOCKS5 version in response"
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
    read_socks5_bind_address(stream, resp_header[3]).await?;
    Ok(())
}

async fn read_socks5_bind_address(
    stream: &mut TcpStream,
    atyp: u8,
) -> Result<()> {
    match atyp {
        SOCKS5_ATYP_IPV4 => {
            let mut addr_port = [0u8; 6];
            timeout(
                Duration::from_secs(10),
                stream.read_exact(&mut addr_port),
            )
                .await
                .context("SOCKS5 IPv4 response read timeout")?
                .context("Failed to read SOCKS5 IPv4 response")?;
            eprintln!("[SOCKS5] IPv4 response: {:?}", addr_port);
        }
        SOCKS5_ATYP_DOMAIN => {
            let mut len_byte = [0u8; 1];
            stream
                .read_exact(&mut len_byte)
                .await
                .context("Failed to read SOCKS5 domain length")?;
            let domain_len = len_byte[0] as usize;
            eprintln!("[SOCKS5] Domain length: {}", domain_len);
            ensure!(
                domain_len <= MAX_HOSTNAME_LEN,
                "Domain too long: {}",
                domain_len
            );
            let mut domain_and_port = vec![0u8; domain_len + 2];
            stream
                .read_exact(&mut domain_and_port)
                .await
                .context("Failed to read SOCKS5 domain response")?;
            eprintln!(
                "[SOCKS5] Domain response: {:?}",
                domain_and_port
            );
        }
        SOCKS5_ATYP_IPV6 => {
            let mut addr_port = [0u8; 18];
            stream
                .read_exact(&mut addr_port)
                .await
                .context("Failed to read SOCKS5 IPv6 response")?;
            eprintln!("[SOCKS5] IPv6 response: {:?}", addr_port);
        }
        atype => {
            bail!("Unsupported address type: {}", atype);
        }
    }
    Ok(())
}
