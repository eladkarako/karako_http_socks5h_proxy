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

// ============================================================================
// SOCKS5 PROTOCOL - ERROR HANDLING
// ============================================================================

fn get_socks5_error_message(error_code: u8) -> String {
    if (error_code as usize) < SOCKS5_ERRORS.len() {
        SOCKS5_ERRORS[error_code as usize].to_string()
    } else {
        format!("Unknown SOCKS5 error code {}", error_code)
    }
}

// ============================================================================
// SOCKS5 PROTOCOL - GREETING
// ============================================================================

async fn socks5_send_greeting(stream: &mut TcpStream) -> Result<()> {
    send_with_timeout(
        stream,
        SOCKS5_GREETING,
        "Failed to send SOCKS5 greeting",
    )
        .await
}

async fn socks5_read_greeting_response(
    stream: &mut TcpStream
) -> Result<()> {
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

async fn socks5_greeting(stream: &mut TcpStream) -> Result<()> {
    socks5_send_greeting(stream).await?;
    socks5_read_greeting_response(stream).await?;
    Ok(())
}

// ============================================================================
// SOCKS5 PROTOCOL - CONNECT REQUEST
// ============================================================================

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

async fn socks5_send_connect_request(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
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
    Ok(())
}

// ============================================================================
// SOCKS5 PROTOCOL - CONNECT RESPONSE
// ============================================================================

async fn socks5_read_connect_response_header(
    stream: &mut TcpStream
) -> Result<[u8; 4]> {
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

    Ok(resp_header)
}

async fn socks5_read_ipv4_address(
    stream: &mut TcpStream
) -> Result<()> {
    let mut addr_port = [0u8; 6];
    stream
        .read_exact(&mut addr_port)
        .await
        .context("Failed to read SOCKS5 IPv4 address response")?;
    Ok(())
}

async fn socks5_read_domain_address(
    stream: &mut TcpStream
) -> Result<()> {
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
    Ok(())
}

async fn socks5_read_ipv6_address(
    stream: &mut TcpStream
) -> Result<()> {
    let mut addr_port = [0u8; 18];
    stream
        .read_exact(&mut addr_port)
        .await
        .context("Failed to read SOCKS5 IPv6 address response")?;
    Ok(())
}

async fn socks5_read_address_response(
    stream: &mut TcpStream,
    atype: u8,
) -> Result<()> {
    match atype {
        1 => socks5_read_ipv4_address(stream).await?,
        3 => socks5_read_domain_address(stream).await?,
        4 => socks5_read_ipv6_address(stream).await?,
        _ => bail!(
            "Unsupported address type in SOCKS5 response: {} (expected 1, 3, or 4)",
            atype
        ),
    }
    Ok(())
}

async fn socks5_read_connect_response(
    stream: &mut TcpStream
) -> Result<()> {
    let resp_header =
        socks5_read_connect_response_header(stream).await?;
    socks5_read_address_response(stream, resp_header[3]).await?;
    Ok(())
}

// ============================================================================
// SOCKS5 PROTOCOL - COMPLETE HANDSHAKE
// ============================================================================

async fn socks5_handshake(
    stream: &mut TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    socks5_greeting(stream).await?;
    socks5_send_connect_request(stream, host, port).await?;
    socks5_read_connect_response(stream).await?;
    Ok(())
}

// ============================================================================
// SOCKS5 UPSTREAM CONNECTION
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
// HTTP Request Header Construction
// ============================================================================

struct HttpRequestHeaderBuilder {
    lines: Vec<String>,
}

impl HttpRequestHeaderBuilder {
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

fn copy_headers_from_request(
    builder: &mut HttpRequestHeaderBuilder,
    headers: &http::HeaderMap,
) {
    for (name, value) in headers.iter() {
        let name_str = name.as_str();
        if !should_skip_header(name_str) {
            if let Ok(value_str) = value.to_str() {
                builder.add_header(name_str, value_str);
            }
        }
    }
}

fn add_host_header(
    builder: &mut HttpRequestHeaderBuilder,
    host: &str,
    port: u16,
) {
    if port == 80 {
        builder.add_header("Host", host);
    } else {
        builder.add_header("Host", &format!("{}:{}", host, port));
    }
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
    let mut builder = HttpRequestHeaderBuilder::new(
        method,
        path_and_query,
        version,
    );
    copy_headers_from_request(&mut builder, headers);
    add_host_header(&mut builder, host, port);
    builder.add_header("Connection", "close");

    if body_len > 0 {
        builder.add_header("Content-Length", &body_len.to_string());
    }

    builder.finalize()
}

// ============================================================================
// HTTP RESPONSE PARSING
// ============================================================================

struct HttpResponse {
    status_code: u16,
    headers: http::HeaderMap,
    body: String,
}

fn parse_status_line(status_line: &str) -> Result<u16> {
    let status_code: u16 = status_line
        .split_whitespace()
        .nth(1)
        .context("Invalid status line")?
        .parse()
        .context("Invalid status code")?;
    Ok(status_code)
}

fn parse_response_headers(
    headers_lines: &[&str]
) -> http::HeaderMap {
    let mut headers = http::HeaderMap::new();

    for line in &headers_lines[1..] {
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

    headers
}

fn parse_http_response(response_text: &str) -> Result<HttpResponse> {
    let parts: Vec<&str> = response_text.split("\r\n\r\n").collect();
    let headers_section = parts[0];
    let body = if parts.len() > 1 { parts[1] } else { "" };

    let lines: Vec<&str> = headers_section.lines().collect();
    let status_line = lines.first().context("Empty response")?;
    let status_code = parse_status_line(status_line)?;
    let headers = parse_response_headers(&lines);

    Ok(HttpResponse { status_code, headers, body: body.to_string() })
}

// ============================================================================
// HTTP REQUEST BODY HANDLING
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

// ============================================================================
// HTTP REQUEST PARSING
// ============================================================================

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

fn extract_path_and_query(uri: &hyper::Uri) -> &str {
    uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
}

// ============================================================================
// HTTP REQUEST PROXYING
// ============================================================================

async fn send_http_request_through_socks5(
    upstream_stream: &mut TcpStream,
    request_headers: &[u8],
    body_bytes: &[u8],
) -> Result<()> {
    upstream_stream.write_all(request_headers).await.context(
        "Failed to send HTTP request headers through SOCKS5 tunnel",
    )?;

    upstream_stream.write_all(body_bytes).await.context(
        "Failed to send HTTP request body through SOCKS5 tunnel",
    )?;

    Ok(())
}

async fn read_http_response_from_socks5(
    upstream_stream: &mut TcpStream
) -> Result<String> {
    let mut response_buf = vec![0u8; RESPONSE_BUFFER_SIZE];
    let n = read_with_timeout(
        upstream_stream,
        &mut response_buf,
        READ_RESPONSE_TIMEOUT_SECS,
    )
        .await?;

    ensure!(n > 0, "Upstream closed connection without response");

    String::from_utf8(response_buf[..n].to_vec())
        .context("Invalid UTF-8 in response")
}

fn build_response_from_parsed(
    parsed_response: HttpResponse
) -> Response<Full<Bytes>> {
    let mut response_builder =
        Response::builder().status(parsed_response.status_code);

    for (name, value) in parsed_response.headers.iter() {
        response_builder = response_builder.header(name, value);
    }

    response_builder
        .body(Full::new(Bytes::from(parsed_response.body)))
        .unwrap()
}

fn build_error_response(
    error: &anyhow::Error
) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::BAD_GATEWAY)
        .body(Full::new(Bytes::from(format!(
            "Proxy error: {}",
            error
        ))))
        .unwrap()
}

// ============================================================================
// HTTP CONNECT METHOD (HTTPS TUNNELING)
// ============================================================================

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

// ============================================================================
// HTTP REQUEST (GET, POST, etc.)
// ============================================================================

async fn process_http_request_through_socks5(
    method: &str,
    path_and_query: &str,
    version: hyper::Version,
    headers: &http::HeaderMap,
    body_bytes: &[u8],
    host: &str,
    port: u16,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, host, port).await?;

    let request_headers = build_http_request_headers(
        method,
        path_and_query,
        version,
        headers,
        host,
        port,
        body_bytes.len(),
    );

    send_http_request_through_socks5(
        &mut upstream_stream,
        &request_headers,
        body_bytes,
    )
        .await?;

    let response_text =
        read_http_response_from_socks5(&mut upstream_stream).await?;
    let parsed_response = parse_http_response(&response_text)?;

    eprintln!("Response status: {}", parsed_response.status_code);

    Ok(build_response_from_parsed(parsed_response))
}

async fn handle_http_get_post(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!("{} {} (host: {}:{})", method, uri, host, port);

    // Read request body
    let body_bytes = read_body(&mut req).await?;
    let path_and_query = extract_path_and_query(&uri);

    process_http_request_through_socks5(
        method.as_str(),
        path_and_query,
        req.version(),
        req.headers(),
        &body_bytes,
        &host,
        port,
        upstream_port,
    )
        .await
}

// ============================================================================
// HTTP REQUEST DISPATCHER
// ============================================================================

async fn handle_http_request(
    req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    if method == hyper::Method::CONNECT {
        return handle_http_connect(&host, port, upstream_port)
            .await;
    }

    handle_http_get_post(req, upstream_port).await
}

// ============================================================================
// CLIENT CONNECTION HANDLING
// ============================================================================

async fn serve_single_client(
    io: hyper_util::rt::TokioIo<tokio::net::TcpStream>,
    upstream_port: u16,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let service =
        hyper::service::service_fn(move |req: Request<Incoming>| {
            let upstream_port = upstream_port;
            async move {
                match handle_http_request(req, upstream_port).await {
                    Ok(response) => Ok::<_, Infallible>(response),
                    Err(e) => {
                        eprintln!("Request handling error: {:#}", e);
                        Ok(build_error_response(&e))
                    }
                }
            }
        });

    hyper::server::conn::http1::Builder::new()
        .serve_connection(io, service)
        .await
        .map_err(|e| {
            Box::new(e) as Box<dyn std::error::Error + Send + Sync>
        })
}

async fn handle_client_connection(
    socket: tokio::net::TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) {
    let io = hyper_util::rt::TokioIo::new(socket);

    if let Err(e) = serve_single_client(io, upstream_port).await {
        eprintln!("HTTP connection error from {}: {}", addr, e);
    }
}

// ============================================================================
// SERVER STARTUP & SIGNAL HANDLING
// ============================================================================

async fn accept_client_connection(
    listener: &tokio::net::TcpListener,
    upstream_port: u16,
) -> Result<()> {
    match listener.accept().await {
        Ok((socket, addr)) => {
            tokio::spawn(async move {
                handle_client_connection(
                    socket,
                    addr,
                    upstream_port,
                )
                    .await;
            });
            Ok(())
        }
        Err(e) => {
            eprintln!("Accept error: {}", e);
            Err(e.into())
        }
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
            result = accept_client_connection(&listener, upstream_port) => {
                result.ok();
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

// ============================================================================
// VALIDATION
// ============================================================================

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

// ============================================================================
// SERVER INITIALIZATION
// ============================================================================

async fn bind_listener(
    http_port: u16
) -> Result<tokio::net::TcpListener> {
    let addr: std::net::SocketAddr =
        format!("127.0.0.1:{}", http_port)
            .parse()
            .context("Failed to parse listen address")?;

    tokio::net::TcpListener::bind(&addr).await.with_context(|| {
        format!(
            "Failed to bind HTTP listener on 127.0.0.1:{}",
            http_port
        )
    })
}

fn log_startup_info(
    http_port: u16,
    upstream_port: u16,
) {
    eprintln!("HTTP proxy listening on 127.0.0.1:{}", http_port);
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        upstream_port
    );
}

fn log_shutdown() {
    eprintln!("Stopped accepting connections.");
}

// ============================================================================
// MAIN SERVER LOOP
// ============================================================================

async fn run_proxy_server(
    http_port: u16,
    upstream_port: u16,
) -> Result<()> {
    validate_ports(http_port, upstream_port)?;

    let listener = bind_listener(http_port).await?;
    log_startup_info(http_port, upstream_port);

    let shutdown = std::sync::Arc::new(tokio::sync::Notify::new());
    setup_signal_handler(shutdown.clone());

    accept_and_handle_connections(listener, upstream_port, shutdown)
        .await?;
    log_shutdown();

    Ok(())
}

// ============================================================================
// ENTRY POINT
// ============================================================================

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    run_proxy_server(args.http_port, args.upstream_port).await
}
