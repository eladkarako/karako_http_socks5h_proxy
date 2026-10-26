#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64"
    )
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

use anyhow::{bail, ensure, Context, Result};
use bytes::Bytes;
use clap::Parser;
use http::{Request, Response, StatusCode};
use http_body_util::Full;

use hyper::body::Incoming;
use std::convert::Infallible;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{timeout, Duration},
};

#[derive(Parser)]
#[command(author, about, long_about = None)]
struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    http_port: u16,

    #[arg(short = 'u', long, default_value = "5678")]
    upstream_port: u16,
}

// ============================================================================
// CONSTANTS
// ============================================================================

const SOCKS5_GREETING: &[u8] = &[5, 1, 0];
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

const MAX_HOSTNAME_LEN: usize = 255;
const SOCKS5_CONNECT_MAX_LEN: usize = 262;
async fn handle_http_request(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!("{} {} (host: {}:{})", method, uri, host, port);

    let body_bytes = read_body(&mut req).await?;
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, &host, port).await?;

    // Build raw HTTP request
    let request_line = format!(
        "{} {} HTTP/{:?}\r\n",
        method,
        req.uri().path_and_query().unwrap_or(&"/".parse().unwrap()),
        req.version()
    );

    let mut request_bytes = request_line.into_bytes();

    for (name, value) in req.headers().iter() {
        let name_str = name.as_str();
        if matches!(
            name_str.to_lowercase().as_str(),
            "host" | "connection" | "content-length"
        ) {
            continue;
        }
        request_bytes.extend_from_slice(name_str.as_bytes());
        request_bytes.extend_from_slice(b": ");
        request_bytes.extend_from_slice(value.as_bytes());
        request_bytes.extend_from_slice(b"\r\n");
    }

    request_bytes.extend_from_slice(b"Host: ");
    request_bytes.extend_from_slice(host.as_bytes());
    if port != 80 {
        request_bytes.extend_from_slice(b":");
        request_bytes.extend_from_slice(port.to_string().as_bytes());
    }
    request_bytes.extend_from_slice(b"\r\n");
    request_bytes.extend_from_slice(b"Connection: close\r\n");

    if !body_bytes.is_empty() {
        request_bytes.extend_from_slice(b"Content-Length: ");
        request_bytes.extend_from_slice(
            body_bytes.len().to_string().as_bytes(),
        );
        request_bytes.extend_from_slice(b"\r\n");
    }

    request_bytes.extend_from_slice(b"\r\n");
    request_bytes.extend_from_slice(&body_bytes);

    upstream_stream.write_all(&request_bytes).await.context(
        "Failed to send HTTP request through SOCKS5 tunnel",
    )?;

    // Read response in chunks, parse headers, relay body as binary
    let mut response_buf = Vec::new();
    let mut buf = [0u8; 8192];

    // Read until we find the end of headers
    let headers_end;
    loop {
        let n = timeout(
            Duration::from_secs(60),
            upstream_stream.read(&mut buf),
        )
            .await
            .context("Timeout reading response from upstream")??;

        ensure!(
            n > 0,
            "Upstream closed connection without response"
        );
        response_buf.extend_from_slice(&buf[..n]);

        // Look for end of headers
        if let Some(pos) =
            response_buf.windows(4).position(|w| w == b"\r\n\r\n")
        {
            headers_end = Some(pos + 4);
            break;
        }

        // Safety check: headers shouldn't be huge
        if response_buf.len() > 65536 {
            bail!("Response headers exceed 64 KB");
        }
    }

    let headers_end = headers_end.unwrap();
    let headers_section =
        std::str::from_utf8(&response_buf[..headers_end - 4])
            .context("Invalid UTF-8 in response headers")?;

    let lines: Vec<&str> = headers_section.lines().collect();
    let status_line = lines.first().context("Empty response")?;
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .context("Invalid status line")?
        .parse()
        .context("Invalid status code")?;

    eprintln!("Response status: {}", status_code);

    let mut response_builder =
        Response::builder().status(status_code);
    for line in &lines[1..] {
        if let Some((key, value)) = line.split_once(':') {
            response_builder =
                response_builder.header(key.trim(), value.trim());
        }
    }

    // Relay remaining body as-is (binary safe)
    let body_start = response_buf[headers_end..].to_vec();

    // Read rest of body from upstream
    let mut full_body = body_start;
    loop {
        match timeout(
            Duration::from_secs(60),
            upstream_stream.read(&mut buf),
        )
            .await
            .context("Timeout reading response body")?
        {
            Ok(0) => break,
            Ok(n) => full_body.extend_from_slice(&buf[..n]),
            Err(e) => return Err(e.into()),
        }
    }

    Ok(response_builder
        .body(Full::new(Bytes::from(full_body)))
        .unwrap())
}
const TUNNEL_BUFFER_SIZE: usize = 65536; // 64 KB instead of 8192

async fn tunnel_bidirectional(
    client: &mut TcpStream,
    upstream: &mut TcpStream,
    client_addr: &std::net::SocketAddr,
    host: &str,
    port: u16,
) -> Result<()> {
    let (mut client_read, mut client_write) = client.split();
    let (mut upstream_read, mut upstream_write) = upstream.split();

    let client_addr_str = client_addr.to_string();
    let host = host.to_string();

    let client_to_upstream = {
        let client_addr_str = client_addr_str.clone();
        let host = host.clone();
        async move {
            let mut buf = vec![0u8; TUNNEL_BUFFER_SIZE];
            let mut total_sent = 0usize;
            loop {
                match client_read.read(&mut buf).await {
                    Ok(0) => {
                        eprintln!(
                            "[{}->{}:{}] Client closed write (sent {} bytes total)",
                            client_addr_str, host, port, total_sent
                        );
                        if let Err(e) =
                            upstream_write.shutdown().await
                        {
                            eprintln!(
                                "[{}->{}:{}] Error shutting down upstream write: {}",
                                client_addr_str, host, port, e
                            );
                        } else {
                            eprintln!(
                                "[{}->{}:{}] Upstream write shut down gracefully",
                                client_addr_str, host, port
                            );
                        }
                        break;
                    }
                    Ok(n) => {
                        total_sent += n;
                        eprintln!(
                            "[{}->{}:{}] Client -> Upstream: {} bytes (total: {})",
                            client_addr_str,
                            host,
                            port,
                            n,
                            total_sent
                        );
                        if let Err(e) =
                            upstream_write.write_all(&buf[..n]).await
                        {
                            eprintln!(
                                "[{}->{}:{}] Error writing to upstream: {}",
                                client_addr_str, host, port, e
                            );
                            let _ = upstream_write.shutdown().await;
                            return Err(e);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[{}->{}:{}] Error reading from client: {}",
                            client_addr_str, host, port, e
                        );
                        let _ = upstream_write.shutdown().await;
                        return Err(e);
                    }
                }
            }
            Ok::<_, std::io::Error>(())
        }
    };

    let upstream_to_client = {
        let client_addr_str = client_addr_str.clone();
        let host = host.clone();
        async move {
            let mut buf = vec![0u8; TUNNEL_BUFFER_SIZE];
            let mut total_received = 0usize;
            loop {
                match upstream_read.read(&mut buf).await {
                    Ok(0) => {
                        eprintln!(
                            "[{}->{}:{}] Upstream closed (received {} bytes total)",
                            client_addr_str,
                            host,
                            port,
                            total_received
                        );
                        if let Err(e) = client_write.shutdown().await
                        {
                            eprintln!(
                                "[{}->{}:{}] Error shutting down client write: {}",
                                client_addr_str, host, port, e
                            );
                        } else {
                            eprintln!(
                                "[{}->{}:{}] Client write shut down gracefully",
                                client_addr_str, host, port
                            );
                        }
                        break;
                    }
                    Ok(n) => {
                        total_received += n;
                        eprintln!(
                            "[{}->{}:{}] Upstream -> Client: {} bytes (total: {})",
                            client_addr_str,
                            host,
                            port,
                            n,
                            total_received
                        );
                        if let Err(e) =
                            client_write.write_all(&buf[..n]).await
                        {
                            eprintln!(
                                "[{}->{}:{}] Error writing to client: {}",
                                client_addr_str, host, port, e
                            );
                            let _ = client_write.shutdown().await;
                            return Err(e);
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[{}->{}:{}] Error reading from upstream: {}",
                            client_addr_str, host, port, e
                        );
                        let _ = client_write.shutdown().await;
                        return Err(e);
                    }
                }
            }
            Ok::<_, std::io::Error>(())
        }
    };

    eprintln!(
        "[{}->{}:{}] Tunnel bidirectional started",
        client_addr_str, host, port
    );
    let result =
        tokio::try_join!(client_to_upstream, upstream_to_client);
    eprintln!(
        "[{}->{}:{}] Tunnel bidirectional ended",
        client_addr_str, host, port
    );
    result.context("Bidirectional tunnel failed")?;
    Ok(())
}

async fn handle_connect_tunnel(
    client: &mut TcpStream,
    host: &str,
    port: u16,
    upstream_port: u16,
    client_addr: std::net::SocketAddr, // Add this parameter
) -> Result<()> {
    eprintln!(
        "CONNECT tunnel requested for {}:{} from {}",
        host, port, client_addr
    );
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, host, port).await?;
    eprintln!("CONNECT tunnel established for {}:{}", host, port);
    client
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await
        .context("Failed to send CONNECT response")?;
    tunnel_bidirectional(
        client,
        &mut upstream_stream,
        &client_addr,
        host,
        port,
    )
        .await?;
    Ok(())
}

fn extract_host(
    uri: &hyper::Uri,
    req: &Request<Incoming>,
) -> Result<String> {
    if let Some(host) = uri.host() {
        return Ok(host.to_string());
    }

    // Try to get from Host header
    if let Some(host_header) = req.headers().get("host") {
        let host_str =
            host_header.to_str().context("Invalid Host header")?;
        if let Some((host, _port)) = host_str.split_once(':') {
            return Ok(host.to_string());
        }
        return Ok(host_str.to_string());
    }

    bail!("Cannot determine host from request")
}

fn extract_port(uri: &hyper::Uri) -> u16 {
    uri.port_u16().unwrap_or(80)
}

async fn read_body(req: &mut Request<Incoming>) -> Result<Vec<u8>> {
    use http_body_util::BodyExt;

    let body = req.body_mut();
    let collected = body
        .collect()
        .await
        .context("Error reading request body")?;
    Ok(collected.to_bytes().to_vec())
}

async fn connect_to_socks5(upstream_port: u16) -> Result<TcpStream> {
    let addr = format!("127.0.0.1:{}", upstream_port);
    let upstream_stream = timeout(Duration::from_secs(30), TcpStream::connect(&addr))
        .await
        .with_context(|| {
            format!(
                "Failed (or 30 seconds timeout) to connect to SOCKS5 server at {}",
                addr
            )
        })??;

    upstream_stream
        .set_nodelay(true)
        .context("Failed to set TCP_NODELAY")?;

    eprintln!("[SOCKS5] Connected to {}", addr);

    Ok(upstream_stream)
}

async fn socks5_handshake(
    upstream_stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    // Send greeting
    timeout(
        Duration::from_secs(30),
        upstream_stream.write_all(SOCKS5_GREETING),
    )
        .await
        .context("Failed to send SOCKS5 greeting")??;

    eprintln!("Sent SOCKS5 greeting: {:?}", SOCKS5_GREETING);

    // Read greeting response
    let mut resp = [0u8; 2];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp),
    )
        .await
        .context("Failed to read SOCKS5 greeting response")??;

    eprintln!("Received SOCKS5 greeting response: {:?}", resp);

    ensure!(
        resp[0] == 5,
        "Invalid SOCKS5 version in greeting response: {} (expected 5)",
        resp[0]
    );
    ensure!(
        resp[1] != 0xff,
        "SOCKS5 server rejected all authentication methods"
    );

    // Build CONNECT request
    ensure!(
        host.len() <= MAX_HOSTNAME_LEN,
        "Hostname too long: {} bytes (max {})",
        host.len(),
        MAX_HOSTNAME_LEN
    );

    let mut connect_req = [0u8; SOCKS5_CONNECT_MAX_LEN];
    let len = 6 + host.len() + 2; // 6-byte header + hostname + 2-byte port

    connect_req[0] = 5; // VER: SOCKS version
    connect_req[1] = 1; // CMD: CONNECT command
    connect_req[2] = 0; // RSV: Reserved
    connect_req[3] = 3; // ATYP: Domain name address type
    connect_req[4] = host.len() as u8; // DOMLEN: Domain name length
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes()); // DOM: Domain name bytes
    connect_req[5 + host.len()] = (port >> 8) as u8; // PORT: High byte (big-endian)
    connect_req[5 + host.len() + 1] = (port & 0xff) as u8; // PORT: Low byte (big-endian)

    eprintln!(
        "Sending SOCKS5 CONNECT for {}:{}: {:?}",
        host,
        port,
        &connect_req[..len]
    );

    // Send CONNECT request
    timeout(
        Duration::from_secs(30),
        upstream_stream.write_all(&connect_req[..len]),
    )
        .await
        .with_context(|| {
            format!(
                "Failed to send SOCKS5 connect request for {}:{}",
                host, port
            )
        })??;

    eprintln!(
        "Waiting for SOCKS5 CONNECT response header (4 bytes)..."
    );

    // Read CONNECT response header
    let mut resp_header = [0u8; 4];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp_header),
    )
        .await
        .context("Failed to read SOCKS5 connect response header")??;

    eprintln!("Received SOCKS5 CONNECT response: {:?}", resp_header);

    ensure!(
        resp_header[0] == 5,
        "Invalid SOCKS5 version in connect response: {} (expected 5)",
        resp_header[0]
    );

    if resp_header[1] != 0 {
        let error_msg = if (resp_header[1] as usize)
            < SOCKS5_ERRORS.len()
        {
            SOCKS5_ERRORS[resp_header[1] as usize].to_string()
        } else {
            format!("Unknown SOCKS5 error code {}", resp_header[1])
        };
        bail!("SOCKS5 connection failed: {}", error_msg);
    }

    // Read bind address (variable length based on address type)
    match resp_header[3] {
        1 => {
            // IPv4
            let mut addr_port = [0u8; 6];
            timeout(
                Duration::from_secs(10),
                upstream_stream.read_exact(&mut addr_port),
            )
                .await
                .context(
                    "Failed to read SOCKS5 IPv4 address response",
                )??;
        }
        3 => {
            // Domain name
            let mut len_byte = [0u8; 1];
            upstream_stream
                .read_exact(&mut len_byte)
                .await
                .context("Failed to read SOCKS5 domain length")?;
            let domain_len = len_byte[0] as usize;
            ensure!(
                domain_len <= MAX_HOSTNAME_LEN,
                "SOCKS5 response domain too long: {} bytes (max {})",
                domain_len,
                MAX_HOSTNAME_LEN
            );
            let mut domain_and_port = vec![0u8; domain_len + 2];
            upstream_stream
                .read_exact(&mut domain_and_port)
                .await
                .context("Failed to read SOCKS5 domain response")?;
        }
        4 => {
            // IPv6
            let mut addr_port = [0u8; 18];
            upstream_stream
                .read_exact(&mut addr_port)
                .await
                .context(
                    "Failed to read SOCKS5 IPv6 address response",
                )?;
        }
        atype => {
            bail!(
                "Unsupported address type in SOCKS5 response: {} (expected 1, 3, or 4)",
                atype
            );
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.http_port != args.upstream_port,
        "HTTP port ({}) cannot be the same as upstream port ({})",
        args.http_port,
        args.upstream_port
    );

    let addr: std::net::SocketAddr =
        format!("127.0.0.1:{}", args.http_port)
            .parse()
            .context("Failed to parse listen address")?;

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| {
            format!(
                "Failed to bind HTTP listener on 127.0.0.1:{}",
                args.http_port
            )
        })?;

    eprintln!(
        "HTTP proxy listening on 127.0.0.1:{}",
        args.http_port
    );
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        args.upstream_port
    );

    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    let shutdown_clone = shutdown.clone();

    let _signal_handle = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nShutting down gracefully...");
        shutdown_clone.notify_waiters();
    });

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            result = listener.accept() => {
                match result {
                    Ok((mut socket, addr)) => {
                        let upstream_port = args.upstream_port;
                        tokio::spawn(async move {
                            // Read first line to detect CONNECT
                            let mut buf = [0u8; 1024];
                            match socket.read(&mut buf).await {
                                Ok(n) if n > 0 => {
                                    let request_str = match std::str::from_utf8(&buf[..n]) {
                                        Ok(s) => s,
                                        Err(_) => {
                                            eprintln!("Invalid UTF-8 from {}", addr);
                                            return;
                                        }
                                    };

                                                                        if let Some(line) = request_str.lines().next() {
                                        let parts: Vec<&str> = line.split_whitespace().collect();
                                        if parts.len() >= 2 && parts[0] == "CONNECT" {
                                            // Handle CONNECT tunnel
                                            let host_port = parts[1];
                                            if let Some((host, port_str)) = host_port.split_once(':') {
                                                if let Ok(port) = port_str.parse::<u16>() {
                                                    if let Err(e) = handle_connect_tunnel(
                                                        &mut socket,
                                                        host,
                                                        port,
                                                        upstream_port,
                                                        addr,
                                                    )
                                                    .await
                                                    {
                                                        eprintln!("CONNECT tunnel error from {}: {:#}", addr, e);
                                                    }
                                                    return;
                                                }
                                            }
                                            let _ = socket.write_all(b"HTTP/1.1 400 Bad Request\r\n\r\n").await;
                                            return;
                                        }
                                    }


                                    // Regular HTTP request - reconstruct and use hyper
                                    let io = hyper_util::rt::TokioIo::new(socket);
                                    let service = hyper::service::service_fn(move |req: Request<Incoming>| {
                                        let upstream_port = upstream_port;
                                        async move {
                                            match handle_http_request(req, upstream_port).await {
                                                Ok(response) => Ok::<_, Infallible>(response),
                                                Err(e) => {
                                                    eprintln!("Request handling error: {:#}", e);
                                                    Ok(Response::builder()
                                                        .status(StatusCode::BAD_GATEWAY)
                                                        .body(Full::new(Bytes::from(format!("Proxy error: {}", e))))
                                                        .unwrap())
                                                }
                                            }
                                        }
                                    });

                                    if let Err(e) = hyper::server::conn::http1::Builder::new()
                                        .serve_connection(io, service)
                                        .await
                                    {
                                        eprintln!("HTTP connection error from {}: {}", addr, e);
                                    }
                                }
                                Ok(_) => {
                                    eprintln!("Empty read from {}", addr);
                                }
                                Err(e) => {
                                    eprintln!("Read error from {}: {}", addr, e);
                                }
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
        }
    }

    eprintln!("Stopped accepting connections.");
    Ok(())
}
