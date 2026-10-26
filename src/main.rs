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
use std::{convert::Infallible, str::FromStr};
use tokio::{io::{AsyncReadExt, AsyncWriteExt}, net::TcpStream, time::{timeout, Duration}};

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
const RESPONSE_BUFFER_SIZE: usize = 65536;
const IO_TIMEOUT_SECS: u64 = 30;
const READ_RESPONSE_TIMEOUT_SECS: u64 = 60;

// ============================================================================
// SOCKS5 Protocol Helpers
// ============================================================================

async fn send_with_timeout(
    stream: &mut TcpStream,
    data: &[u8],
    context_msg: &str,
) -> Result<()> {
    timeout(
        Duration::from_secs(IO_TIMEOUT_SECS),
        stream.write_all(data),
    )
        .await
        .with_context(|| format!("{} (30s timeout)", context_msg))??;
    Ok(())
}

async fn read_exact_with_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    context_msg: &str,
) -> Result<()> {
    timeout(
        Duration::from_secs(IO_TIMEOUT_SECS),
        stream.read_exact(buf),
    )
        .await
        .with_context(|| format!("{} (30s timeout)", context_msg))??;
    Ok(())
}

async fn read_with_timeout(
    stream: &mut TcpStream,
    buf: &mut [u8],
    timeout_secs: u64,
) -> Result<usize> {
    timeout(Duration::from_secs(timeout_secs), stream.read(buf))
        .await
        .context("Timeout reading from stream")?
        .context("Failed to read from stream")
}

fn get_socks5_error_message(error_code: u8) -> String {
    if (error_code as usize) < SOCKS5_ERRORS.len() {
        SOCKS5_ERRORS[error_code as usize].to_string()
    } else {
        format!("Unknown SOCKS5 error code {}", error_code)
    }
}

// ============================================================================
// SOCKS5 Handshake & Connection
// ============================================================================

async fn socks5_greeting(stream: &mut TcpStream) -> Result<()> {
    send_with_timeout(
        stream,
        SOCKS5_GREETING,
        "Failed to send SOCKS5 greeting",
    )
        .await?;

    let mut resp = [0u8; 2];
    read_exact_with_timeout(
        stream,
        &mut resp,
        "Failed to read SOCKS5 greeting response",
    )
        .await?;

    ensure!(
        resp[0] == 5,
        "Invalid SOCKS5 version in greeting response: {} (expected 5)",
        resp[0]
    );
    ensure!(
        resp[1] != 0xff,
        "SOCKS5 server rejected all authentication methods"
    );

    Ok(())
}

fn build_socks5_connect_request(
    host: &str,
    port: u16,
) -> Result<Vec<u8>> {
    ensure!(
        host.len() <= MAX_HOSTNAME_LEN,
        "Hostname too long: {} bytes (max {})",
        host.len(),
        MAX_HOSTNAME_LEN
    );

    let mut request = vec![0u8; 6 + host.len()];
    request[0] = 5; // SOCKS version
    request[1] = 1; // CONNECT command
    request[2] = 0; // Reserved
    request[3] = 3; // Domain name address type
    request[4] = host.len() as u8;
    request[5..5 + host.len()].copy_from_slice(host.as_bytes());
    request[5 + host.len()] = (port >> 8) as u8;
    request[5 + host.len() + 1] = (port & 0xff) as u8;

    Ok(request)
}

async fn socks5_connect_response(
    stream: &mut TcpStream
) -> Result<()> {
    let mut resp_header = [0u8; 4];
    read_exact_with_timeout(
        stream,
        &mut resp_header,
        "Failed to read SOCKS5 connect response header",
    )
        .await?;

    ensure!(
        resp_header[0] == 5,
        "Invalid SOCKS5 version in connect response: {} (expected 5)",
        resp_header[0]
    );

    if resp_header[1] != 0 {
        let error_msg = get_socks5_error_message(resp_header[1]);
        bail!("SOCKS5 connection failed: {}", error_msg);
    }

    handle_socks5_address_response(stream, resp_header[3]).await?;
    Ok(())
}

async fn handle_socks5_address_response(
    stream: &mut TcpStream,
    atype: u8,
) -> Result<()> {
    match atype {
        1 => {
            // IPv4 address (4 bytes) + port (2 bytes)
            let mut addr_port = [0u8; 6];
            stream.read_exact(&mut addr_port).await.context(
                "Failed to read SOCKS5 IPv4 address response",
            )?;
        }
        3 => {
            // Domain name
            let mut len_byte = [0u8; 1];
            stream
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
            stream
                .read_exact(&mut domain_and_port)
                .await
                .context("Failed to read SOCKS5 domain response")?;
        }
        4 => {
            // IPv6 address (16 bytes) + port (2 bytes)
            let mut addr_port = [0u8; 18];
            stream.read_exact(&mut addr_port).await.context(
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

async fn socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    socks5_greeting(stream).await?;

    let connect_req = build_socks5_connect_request(host, port)?;
    send_with_timeout(
        stream,
        &connect_req,
        &format!(
            "Failed to send SOCKS5 connect request for {}:{}",
            host, port
        ),
    )
        .await?;

    socks5_connect_response(stream).await?;
    Ok(())
}

// ============================================================================
// Network Connection
// ============================================================================

async fn connect_to_socks5(upstream_port: u16) -> Result<TcpStream> {
    let addr = format!("127.0.0.1:{}", upstream_port);
    let upstream_stream = timeout(
        Duration::from_secs(IO_TIMEOUT_SECS),
        TcpStream::connect(&addr),
    )
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

// ============================================================================
// Request Header Construction
// ============================================================================

struct RequestHeaderBuilder {
    lines: Vec<String>,
}

impl RequestHeaderBuilder {
    fn new(
        method: &str,
        path_and_query: &str,
        version: hyper::Version,
    ) -> Self {
        let version_str = match version {
            hyper::Version::HTTP_10 => "1.0",
            hyper::Version::HTTP_11 => "1.1",
            _ => "1.1",
        };
        let request_line = format!(
            "{} {} HTTP/{}\r\n",
            method, path_and_query, version_str
        );

        Self { lines: vec![request_line] }
    }

    fn add_header(
        &mut self,
        name: &str,
        value: &str,
    ) {
        self.lines.push(format!("{}: {}\r\n", name, value));
    }

    fn finalize(mut self) -> Vec<u8> {
        self.lines.push("\r\n".to_string());
        self.lines.join("").into_bytes()
    }
}

fn should_skip_header(name: &str) -> bool {
    let lower = name.to_lowercase();
    matches!(
        lower.as_str(),
        "host" | "connection" | "content-length"
    )
}

fn build_http_request_headers(
    method: &str,
    path_and_query: &str,
    version: hyper::Version,
    headers: &http::HeaderMap,
    host: &str,
    port: u16,
    body_len: usize,
) -> Vec<u8> {
    let mut builder =
        RequestHeaderBuilder::new(method, path_and_query, version);

    // Copy relevant headers from original request
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        if !should_skip_header(name_str) {
            if let Ok(value_str) = value.to_str() {
                builder.add_header(name_str, value_str);
            }
        }
    }

    // Add Host header
    if port == 80 {
        builder.add_header("Host", host);
    } else {
        builder.add_header("Host", &format!("{}:{}", host, port));
    }

    // Add Connection header
    builder.add_header("Connection", "close");

    // Add Content-Length if body exists
    if body_len > 0 {
        builder.add_header("Content-Length", &body_len.to_string());
    }

    builder.finalize()
}

// ============================================================================
// Response Parsing
// ============================================================================

struct HttpResponse {
    status_code: u16,
    headers: http::HeaderMap,
    body: String,
}
fn parse_http_response(response_text: &str) -> Result<HttpResponse> {
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

    let mut headers = http::HeaderMap::new();
    for line in &lines[1..] {
        if let Some((key, value)) = line.split_once(':') {
            match http::HeaderName::from_str(key.trim()) {
                Ok(header_name) => {
                    match http::HeaderValue::from_str(value.trim()) {
                        Ok(header_value) => {
                            headers
                                .insert(header_name, header_value);
                        }
                        Err(_) => {
                            eprintln!(
                                "Skipping invalid header value: {}",
                                value.trim()
                            );
                        }
                    }
                }
                Err(_) => {
                    eprintln!(
                        "Skipping invalid header name: {}",
                        key.trim()
                    );
                }
            }
        }
    }

    Ok(HttpResponse { status_code, headers, body: body.to_string() })
}

// ============================================================================
// HTTP Request Handling
// ============================================================================

async fn read_body(req: &mut Request<Incoming>) -> Result<Vec<u8>> {
    use http_body_util::BodyExt;
    let body = req.body_mut();
    let collected = body
        .collect()
        .await
        .context("Error reading request body")?;
    Ok(collected.to_bytes().to_vec())
}

fn extract_host(
    uri: &hyper::Uri,
    req: &Request<Incoming>,
) -> Result<String> {
    if let Some(host) = uri.host() {
        return Ok(host.to_string());
    }
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

async fn handle_http_connect(
    host: &str,
    port: u16,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    eprintln!("CONNECT tunnel requested for {}:{}", host, port);

    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, host, port).await?;

    eprintln!("CONNECT tunnel established for {}:{}", host, port);

    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Full::new(Bytes::new()))
        .unwrap())
}

async fn handle_http_request(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!("{} {} (host: {}:{})", method, uri, host, port);

    // Handle CONNECT tunnel
    if method == hyper::Method::CONNECT {
        return handle_http_connect(&host, port, upstream_port)
            .await;
    }

    // Read request body
    let body_bytes = read_body(&mut req).await?;

    // Connect to SOCKS5 server
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, &host, port).await?;

    // Build and send HTTP request
    let path_and_query = req
        .uri()
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");

    let request_headers = build_http_request_headers(
        method.as_str(),
        path_and_query,
        req.version(),
        req.headers(),
        &host,
        port,
        body_bytes.len(),
    );

    let mut full_request = request_headers;
    full_request.extend_from_slice(&body_bytes);

    upstream_stream.write_all(&full_request).await.context(
        "Failed to send HTTP request through SOCKS5 tunnel",
    )?;

    // Read response
    let mut response_buf = vec![0u8; RESPONSE_BUFFER_SIZE];
    let n = read_with_timeout(
        &mut upstream_stream,
        &mut response_buf,
        READ_RESPONSE_TIMEOUT_SECS,
    )
        .await?;

    ensure!(n > 0, "Upstream closed connection without response");

    let response_text =
        String::from_utf8(response_buf[..n].to_vec())
            .context("Invalid UTF-8 in response")?;

    let parsed_response = parse_http_response(&response_text)?;

    eprintln!("Response status: {}", parsed_response.status_code);

    let mut response_builder =
        Response::builder().status(parsed_response.status_code);

    for (name, value) in parsed_response.headers.iter() {
        response_builder = response_builder.header(name, value);
    }

    Ok(response_builder
        .body(Full::new(Bytes::from(parsed_response.body)))
        .unwrap())
}

// ============================================================================
// Server & Connection Handling
// ============================================================================

async fn handle_client_connection(
    socket: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) {
    let io = hyper_util::rt::TokioIo::new(socket);
    let service =
        hyper::service::service_fn(move |req: Request<Incoming>| {
            let upstream_port = upstream_port;
            async move {
                match handle_http_request(req, upstream_port).await {
                    Ok(response) => Ok::<_, Infallible>(response),
                    Err(e) => {
                        eprintln!("Request handling error: {:#}", e);
                        Ok(Response::builder()
                            .status(StatusCode::BAD_GATEWAY)
                            .body(Full::new(Bytes::from(format!(
                                "Proxy error: {}",
                                e
                            ))))
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

async fn accept_and_handle_connections(
    listener: tokio::net::TcpListener,
    upstream_port: u16,
    shutdown: std::sync::Arc<tokio::sync::Notify>,
) -> Result<()> {
    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            result = listener.accept() => {
                match result {
                    Ok((socket, addr)) => {
                        tokio::spawn(async move {
                            handle_client_connection(socket, addr, upstream_port).await;
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                    }
                }
            }
        }
    }

    Ok(())
}

fn setup_signal_handler(
    shutdown: std::sync::Arc<tokio::sync::Notify>
) {
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nShutting down gracefully...");
        shutdown_clone.notify_waiters();
    });
}

fn validate_ports(
    http_port: u16,
    upstream_port: u16,
) -> Result<()> {
    ensure!(
        http_port != upstream_port,
        "HTTP port ({}) cannot be the same as upstream port ({})",
        http_port,
        upstream_port
    );
    Ok(())
}

async fn run_proxy_server(
    http_port: u16,
    upstream_port: u16,
) -> Result<()> {
    validate_ports(http_port, upstream_port)?;

    let addr: std::net::SocketAddr =
        format!("127.0.0.1:{}", http_port)
            .parse()
            .context("Failed to parse listen address")?;

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| {
            format!(
                "Failed to bind HTTP listener on 127.0.0.1:{}",
                http_port
            )
        })?;

    eprintln!("HTTP proxy listening on 127.0.0.1:{}", http_port);
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        upstream_port
    );

    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    setup_signal_handler(shutdown.clone());

    accept_and_handle_connections(listener, upstream_port, shutdown)
        .await?;

    eprintln!("Stopped accepting connections.");
    Ok(())
}

// ============================================================================
// Main Entry Point
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    run_proxy_server(args.http_port, args.upstream_port).await
}
