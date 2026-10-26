use anyhow::{bail, ensure, Context, Result};
use bytes::Bytes;
use clap::Parser;
use http::{Request, Response, StatusCode};
use http_body_util::Full;

use hyper::body::Incoming;
use std::convert::Infallible;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

#[derive(Parser)]
#[command(author, about, long_about = None)]
struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    http_port: u16,

    #[arg(short = 'u', long, default_value = "5678")]
    upstream_port: u16,
}

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

    // Handle CONNECT method for HTTPS tunneling
    if method == hyper::Method::CONNECT {
        return handle_connect_tunnel(&host, port, upstream_port)
            .await;
    }

    // For regular HTTP requests, read the entire body first
    let body_bytes = read_body(&mut req).await?;

    // Connect to upstream SOCKS5h server and perform handshake
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, &host, port).await?;

    // Build the HTTP request to send through the tunnel
    let request_line = format!(
        "{} {} HTTP/{:?}\r\n",
        method,
        req.uri().path_and_query().unwrap_or(&"/".parse().unwrap()),
        req.version()
    );
    let mut request_headers = request_line.into_bytes();

    // Write headers (excluding host and connection headers which we'll set)
    for (name, value) in req.headers().iter() {
        let name_str = name.as_str();
        if name_str.to_lowercase() == "host"
            || name_str.to_lowercase() == "connection"
            || name_str.to_lowercase() == "content-length"
        {
            continue;
        }
        request_headers.extend_from_slice(name_str.as_bytes());
        request_headers.extend_from_slice(b": ");
        request_headers.extend_from_slice(value.as_bytes());
        request_headers.extend_from_slice(b"\r\n");
    }

    // Add required headers
    request_headers.extend_from_slice(b"Host: ");
    request_headers.extend_from_slice(host.as_bytes());
    if port != 80 {
        request_headers.extend_from_slice(b":");
        request_headers
            .extend_from_slice(port.to_string().as_bytes());
    }
    request_headers.extend_from_slice(b"\r\n");
    request_headers.extend_from_slice(b"Connection: close\r\n");

    if !body_bytes.is_empty() {
        request_headers.extend_from_slice(b"Content-Length: ");
        request_headers.extend_from_slice(
            body_bytes.len().to_string().as_bytes(),
        );
        request_headers.extend_from_slice(b"\r\n");
    }

    request_headers.extend_from_slice(b"\r\n");
    if !body_bytes.is_empty() {
        request_headers.extend_from_slice(&body_bytes);
    }

    // Send the complete HTTP request through SOCKS5 tunnel
    upstream_stream.write_all(&request_headers).await.context(
        "Failed to send HTTP request through SOCKS5 tunnel",
    )?;

    // Read the response
    let mut response_buf = vec![0u8; 65536];
    let n = timeout(
        Duration::from_secs(60),
        upstream_stream.read(&mut response_buf),
    )
        .await
        .context("Timeout reading response from upstream")??;

    ensure!(n > 0, "Upstream closed connection without response");

    // Parse the HTTP response
    let response_text =
        String::from_utf8(response_buf[..n].to_vec())
            .context("Invalid UTF-8 in response")?;

    // Simple HTTP response parsing (split headers and body)
    let parts: Vec<&str> = response_text.split("\r\n\r\n").collect();
    let headers_section = parts[0];
    let body = if parts.len() > 1 { parts[1] } else { "" };

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

    // Copy response headers (skip status line)
    for line in &lines[1..] {
        if let Some((key, value)) = line.split_once(':') {
            response_builder =
                response_builder.header(key.trim(), value.trim());
        }
    }

    Ok(response_builder
        .body(Full::new(Bytes::from(body.to_string())))
        .unwrap())
}

async fn handle_connect_tunnel(
    host: &str,
    port: u16,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    eprintln!("CONNECT tunnel requested for {}:{}", host, port);

    // For CONNECT, we need to establish a TCP tunnel
    // This is typically used for HTTPS
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, host, port).await?;

    eprintln!("CONNECT tunnel established for {}:{}", host, port);

    // Return 200 Connection Established
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(Bytes::new()))
        .unwrap())
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

    // Read greeting response
    let mut resp = [0u8; 2];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp),
    )
        .await
        .context("Failed to read SOCKS5 greeting response")??;

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
    let len = 6 + host.len();

    connect_req[0] = 5; // SOCKS version
    connect_req[1] = 1; // CONNECT command
    connect_req[2] = 0; // Reserved
    connect_req[3] = 3; // DOMAINNAME address type
    connect_req[4] = host.len() as u8;
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes());
    connect_req[5 + host.len()] = (port >> 8) as u8;
    connect_req[5 + host.len() + 1] = (port & 0xff) as u8;

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

    // Read CONNECT response header
    let mut resp_header = [0u8; 4];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp_header),
    )
        .await
        .context("Failed to read SOCKS5 connect response header")??;

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
                    Ok((socket, addr)) => {
                        let upstream_port = args.upstream_port;
                        tokio::spawn(async move {
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
