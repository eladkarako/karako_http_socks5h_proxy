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
use http_body_util::{BodyExt, Full};
use hyper::{body::Incoming, service::Service};
use std::{convert::Infallible, pin::Pin, sync::Arc};
use tokio::{
    io::{copy_bidirectional, AsyncReadExt, AsyncWriteExt},
    net::TcpStream,
    time::{timeout, Duration},
};

#[cfg(target_os = "windows")]
const EOL: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
const EOL: &str = "\n";

const SOCKS5_VERSION: u8 = 5;
const SOCKS5_CMD_CONNECT: u8 = 1;
const SOCKS5_ATYP_DOMAIN: u8 = 3;
const SOCKS5_ATYP_IPV4: u8 = 1;
const SOCKS5_ATYP_IPV6: u8 = 4;
const MAX_HOSTNAME_LEN: usize = 255;
const SOCKS5_HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(30);
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(600); // 10 min for tunnels

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

#[derive(Parser, Debug, Clone)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"), long_about = None)]
#[command(
    after_help = concat!("Repository:    ", env!("CARGO_PKG_REPOSITORY"))
)]
struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    http_port: u16,
    #[arg(short = 'u', long, default_value = "5678")]
    upstream_port: u16,
}

#[derive(Clone)]
struct Socks5Http2ProxyService {
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
}

impl Service<Request<Incoming>> for Socks5Http2ProxyService {
    type Response = Response<Full<Bytes>>;
    type Error = Infallible;
    type Future = Pin<
        Box<
            dyn std::future::Future<
                Output=Result<Self::Response, Self::Error>,
            > + Send
            + 'static,
        >,
    >;

    fn call(
        &self,
        req: Request<Incoming>,
    ) -> Self::Future {
        let upstream_port = self.upstream_port;
        let client_addr = self.client_addr;

        Box::pin(async move {
            match handle_h2_request(req, upstream_port, client_addr)
                .await
            {
                Ok(response) => Ok(response),
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
        })
    }
}

/// Main HTTP/2 request handler
async fn handle_h2_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!(
        "{} {} (host: {}:{}) from {}",
        method, uri, host, port, client_addr
    );

    match method.as_str() {
        "CONNECT" => {
            // For CONNECT in HTTP/2, we cannot actually tunnel at the HTTP/2 layer
            // This is a limitation of HTTP/2; CONNECT is primarily for HTTP/1.1
            bail!(
                "CONNECT method not supported over HTTP/2; use HTTP/1.1"
            )
        }
        _ => {
            handle_http_request(req, &host, port, upstream_port)
                .await
        }
    }
}

/// Handle regular HTTP requests (GET, POST, etc.)
async fn handle_http_request(
    mut req: Request<Incoming>,
    host: &str,
    port: u16,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let body_bytes = read_request_body(&mut req).await?;
    let forward_req = build_forward_request(
        &method,
        &uri,
        host,
        port,
        &req,
        &body_bytes,
    )?;

    let mut upstream_stream =
        connect_to_socks5_server(upstream_port).await?;
    establish_socks5_connection(&mut upstream_stream, host, port)
        .await?;

    let response =
        send_http_request(upstream_stream, forward_req).await?;
    Ok(response)
}

/// Extract hostname from URI or Host header
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

/// Extract port from URI, defaults to 80
fn extract_port(uri: &hyper::Uri) -> u16 {
    uri.port_u16().unwrap_or(80)
}

/// Read entire request body into bytes
async fn read_request_body(
    req: &mut Request<Incoming>
) -> Result<Vec<u8>> {
    use http_body_util::BodyExt;
    let body = req.body_mut();
    let collected = body
        .collect()
        .await
        .context("Error reading request body")?;
    Ok(collected.to_bytes().to_vec())
}

/// Build a forward HTTP request with proper headers
fn build_forward_request(
    method: &http::Method,
    uri: &hyper::Uri,
    host: &str,
    port: u16,
    req: &Request<Incoming>,
    body_bytes: &[u8],
) -> Result<Request<Full<Bytes>>> {
    let path_query = uri
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| "/".to_string());
    let forward_url =
        format!("http://{}:{}{}", host, port, path_query);
    let mut forward_req = Request::builder()
        .method(method.clone())
        .uri(forward_url)
        .version(hyper::Version::HTTP_11);

    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if should_forward_header(&name_str) {
            forward_req = forward_req.header(name, value);
        }
    }

    forward_req =
        forward_req.header("host", format!("{}:{}", host, port));
    if !body_bytes.is_empty() {
        forward_req =
            forward_req.header("content-length", body_bytes.len());
    }

    forward_req
        .body(Full::new(Bytes::from(body_bytes.to_vec())))
        .context("Failed to build forward request")
}

/// Determine if a header should be forwarded
fn should_forward_header(name: &str) -> bool {
    !matches!(
        name,
        "host"
            | "connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
            | "keep-alive"
    )
}

/// Connect to local SOCKS5 server
async fn connect_to_socks5_server(
    upstream_port: u16
) -> Result<TcpStream> {
    let addr = format!("127.0.0.1:{}", upstream_port);
    let stream = timeout(
        SOCKS5_HANDSHAKE_TIMEOUT,
        TcpStream::connect(&addr),
    )
        .await
        .with_context(|| {
            format!(
                "Failed (or {:?} timeout) to connect to SOCKS5 server at {}",
                SOCKS5_HANDSHAKE_TIMEOUT, addr
            )
        })?
        .context("SOCKS5 connection failed")?;

    stream.set_nodelay(true).context("Failed to set TCP_NODELAY")?;
    eprintln!("[SOCKS5] Connected to {}", addr);
    Ok(stream)
}

/// Establish SOCKS5 connection to target host
async fn establish_socks5_connection(
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

/// Send SOCKS5 initial greeting
async fn send_socks5_greeting(stream: &mut TcpStream) -> Result<()> {
    let greeting = [SOCKS5_VERSION, 1, 0];
    timeout(SOCKS5_HANDSHAKE_TIMEOUT, stream.write_all(&greeting))
        .await
        .context("SOCKS5 greeting write timeout")?
        .context("Failed to send SOCKS5 greeting")?;
    eprintln!("[SOCKS5] Sent greeting: {:?}", greeting);
    Ok(())
}

/// Receive and validate SOCKS5 greeting response
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

/// Send SOCKS5 CONNECT request
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
    let mut connect_req = vec![0u8; 5 + host.len() + 2]; // allocates 17 bytes
    connect_req[0] = SOCKS5_VERSION; // 1. VER (SOCKS version)
    connect_req[1] = SOCKS5_CMD_CONNECT; // 2. CMD (CONNECT)
    connect_req[2] = 0; // 3. RSV (reserved)
    connect_req[3] = SOCKS5_ATYP_DOMAIN; // 4. ATYP (address type)
    connect_req[4] = host.len() as u8; // 5. DLEN (domain length)
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes()); // hostname (10 for github.com)
    connect_req[5 + host.len()] = (port >> 8) as u8; // 6. + 7. Port Encoding in Big-Endian Format (16-bit)
    connect_req[6 + host.len()] = (port & 0xff) as u8; // 8080 -> 0x1F90 -> [0x1F, 0x90]

    // DEBUG: Print exact request bytes
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

    // Print in readable format
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

    // Print breakdown
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

/// Receive and validate SOCKS5 CONNECT response
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

/// Read SOCKS5 bind address response
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

/// Send HTTP request through SOCKS5 tunnel and get response
async fn send_http_request(
    upstream_stream: TcpStream,
    forward_req: Request<Full<Bytes>>,
) -> Result<Response<Full<Bytes>>> {
    let io = hyper_util::rt::TokioIo::new(upstream_stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake(io)
        .await
        .context("Failed to establish HTTP/1.1 connection through SOCKS5")?;

    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Upstream HTTP/1.1 connection error: {}", e);
        }
    });

    let response = sender
        .send_request(forward_req)
        .await
        .context("Failed to send HTTP/1.1 request through SOCKS5")?;

    let (parts, body) = response.into_parts();
    let body_bytes = http_body_util::BodyExt::collect(body)
        .await
        .context("Failed to read response body")?
        .to_bytes();

    let mut response_builder =
        Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str().to_lowercase();
        if should_forward_header(&name_str) {
            response_builder = response_builder.header(name, value);
        }
    }

    Ok(response_builder
        .body(Full::new(body_bytes))
        .context("Failed to build response")?)
}

/// Handle incoming HTTP/1.1 client connection with CONNECT support
async fn handle_http1_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    eprintln!("[HTTP/1.1 CLIENT] New connection from {}", addr);
    let io = hyper_util::rt::TokioIo::new(socket);

    let service =
        Socks5Http1ProxyService { upstream_port, client_addr: addr };

    match hyper::server::conn::http1::Builder::new()
        .preserve_header_case(true)
        .title_case_headers(false)
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        Ok(_) => {
            eprintln!(
                "[HTTP/1.1 CLIENT] Connection {} closed normally",
                addr
            );
            Ok(())
        }
        Err(e) => {
            if !e.to_string().contains("closed") {
                eprintln!(
                    "[HTTP/1.1 CLIENT] Connection {} error: {}",
                    addr, e
                );
            }
            Ok(())
        }
    }
}

/// HTTP/1.1 service with CONNECT tunnel support
#[derive(Clone)]
struct Socks5Http1ProxyService {
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
}

impl Service<Request<Incoming>> for Socks5Http1ProxyService {
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<
        Box<
            dyn std::future::Future<
                Output=Result<Self::Response, Self::Error>,
            > + Send
            + 'static,
        >,
    >;

    fn call(
        &self,
        req: Request<Incoming>,
    ) -> Self::Future {
        let upstream_port = self.upstream_port;
        let client_addr = self.client_addr;

        Box::pin(async move {
            match handle_http1_request(
                req,
                upstream_port,
                client_addr,
            )
                .await
            {
                Ok(response) => Ok(response),
                Err(e) => {
                    eprintln!(
                        "HTTP/1.1 Request handling error: {:#}",
                        e
                    );
                    Ok(Response::builder()
                        .status(StatusCode::BAD_GATEWAY)
                        .body(BoxBody::new(
                            Full::new(Bytes::from(format!(
                                "Proxy error: {}",
                                e
                            )))
                                .boxed(),
                        ))
                        .unwrap())
                }
            }
        })
    }
}

type BoxBody =
http_body_util::combinators::BoxBody<Bytes, Infallible>;
/// Handle HTTP/1.1 request including CONNECT method with upgrade support
async fn handle_http1_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> Result<Response<BoxBody>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!(
        "[HTTP/1.1] {} {} (host: {}:{}) from {}",
        method, uri, host, port, client_addr
    );

    match method.as_str() {
        "CONNECT" => {
            eprintln!(
                "[CONNECT TUNNEL] Initiating tunnel to {}:{}",
                host, port
            );

            // Connect through SOCKS5 to target
            let target_stream =
                connect_to_socks5_server(upstream_port).await?;
            let mut target_stream_clone = target_stream;
            establish_socks5_connection(
                &mut target_stream_clone,
                &host,
                port,
            )
                .await?;

            eprintln!(
                "[CONNECT TUNNEL] SOCKS5 connection established, sending 200 OK"
            );

            // Create response with 200 Connection Established
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("connection", "upgrade")
                .body(BoxBody::new(Full::new(Bytes::new()).boxed()))
                .context("Failed to build CONNECT response")?;

            // Spawn upgrade handler in background
            // This handles the transition from HTTP to raw tunnel mode
            tokio::spawn(async move {
                match hyper::upgrade::on(req).await {
                    Ok(upgraded) => {
                        eprintln!(
                            "[CONNECT TUNNEL] Upgrade successful for {}:{}",
                            host, port
                        );
                        if let Err(e) = handle_tunnel(
                            upgraded,
                            target_stream_clone,
                            &host,
                            port,
                        )
                            .await
                        {
                            eprintln!(
                                "[CONNECT TUNNEL] Tunnel error for {}:{}: {:#}",
                                host, port, e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[CONNECT TUNNEL] Upgrade failed for {}:{}: {}",
                            host, port, e
                        );
                    }
                }
            });

            Ok(response)
        }
        _ => {
            // Regular HTTP request (GET, POST, etc.)
            let mut req_clone = req;
            let body_bytes =
                read_request_body(&mut req_clone).await?;
            let forward_req = build_forward_request(
                &method,
                &uri,
                &host,
                port,
                &req_clone,
                &body_bytes,
            )?;

            let upstream_stream =
                connect_to_socks5_server(upstream_port).await?;
            let mut upstream_stream_clone = upstream_stream;
            establish_socks5_connection(
                &mut upstream_stream_clone,
                &host,
                port,
            )
                .await?;

            let response = send_http_request(
                upstream_stream_clone,
                forward_req,
            )
                .await?;

            // Convert Response<Full<Bytes>> to Response<BoxBody>
            let (parts, body) = response.into_parts();
            let body_bytes = http_body_util::BodyExt::collect(body)
                .await
                .context("Failed to read response body")?
                .to_bytes();

            Ok(Response::from_parts(
                parts,
                BoxBody::new(Full::new(body_bytes).boxed()),
            ))
        }
    }
}

/// Handle upgraded CONNECT tunnel with bidirectional data forwarding
async fn handle_tunnel(
    upgraded: hyper::upgrade::Upgraded,
    target: TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    eprintln!(
        "[TUNNEL] Starting bidirectional tunnel for {}:{}",
        host, port
    );

    // Wrap the upgraded connection in TokioIo for async I/O
    let mut client = hyper_util::rt::TokioIo::new(upgraded);
    let mut target = target;

    // Set TCP options
    target
        .set_nodelay(true)
        .context("Failed to set TCP_NODELAY on target")?;

    // Bidirectional copy with timeout
    let result = timeout(
        TUNNEL_TIMEOUT,
        copy_bidirectional(&mut client, &mut target),
    )
        .await;

    match result {
        Ok(Ok((client_to_target, target_to_client))) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} closed normally: client→target: {} bytes, target→client: {} bytes",
                host, port, client_to_target, target_to_client
            );
            Ok(())
        }
        Ok(Err(e)) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} copy error: {}",
                host, port, e
            );
            Err(e).context("Bidirectional copy failed")
        }
        Err(_) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} timeout after {:?}",
                host, port, TUNNEL_TIMEOUT
            );
            bail!("Tunnel timeout")
        }
    }
}

/// Handle incoming HTTP/2 client connection
async fn handle_http2_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    eprintln!("[HTTP/2 CLIENT] New connection from {}", addr);
    let io = hyper_util::rt::TokioIo::new(socket);

    let service =
        Socks5Http2ProxyService { upstream_port, client_addr: addr };

    match hyper::server::conn::http2::Builder::new(
        hyper_util::rt::TokioExecutor::new(),
    )
        .serve_connection(io, service)
        .await
    {
        Ok(_) => {
            eprintln!(
                "[HTTP/2 CLIENT] Connection {} closed normally",
                addr
            );
            Ok(())
        }
        Err(e) => {
            if !e.to_string().contains("closed") {
                eprintln!(
                    "[HTTP/2 CLIENT] Connection {} error: {}",
                    addr, e
                );
            }
            Ok(())
        }
    }
}

/// Detect HTTP version and route to appropriate handler
async fn handle_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    let mut peek_buf = [0u8; 24];
    match socket.peek(&mut peek_buf).await {
        Ok(n) if n >= 4 => {
            let peek_str =
                String::from_utf8_lossy(&peek_buf[..n.min(24)]);
            if peek_str.starts_with("PRI * HTTP/2.0") {
                eprintln!(
                    "[DETECT] HTTP/2 preface detected for {}",
                    addr
                );
                handle_http2_client_connection(
                    socket,
                    addr,
                    upstream_port,
                )
                    .await
            } else if peek_str.starts_with("HTTP/")
                || peek_str.contains("CONNECT ")
            {
                eprintln!("[DETECT] HTTP/1.1 detected for {}", addr);
                handle_http1_client_connection(
                    socket,
                    addr,
                    upstream_port,
                )
                    .await
            } else {
                bail!(
                    "Unknown protocol from {}: {:?}",
                    addr,
                    &peek_buf[..n.min(16)]
                )
            }
        }
        Ok(n) if n > 0 => {
            bail!(
                "Failed to peek protocol from {} (got {} bytes, need >= 4)",
                addr,
                n
            )
        }
        Ok(_) => {
            eprintln!(
                "[DETECT] Empty connection from {} (closed immediately)",
                addr
            );
            Ok(())
        }
        Err(e) => {
            bail!("Failed to peek protocol from {}: {}", addr, e)
        }
    }
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
        "HTTP/1.1 + HTTP/2 proxy listening on 127.0.0.1:{}",
        args.http_port
    );
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        args.upstream_port
    );

    let shutdown = Arc::new(tokio::sync::Notify::new());

    // Handle Ctrl+C (any OS)
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!(
            "{}Ctrl+C received, shutting down gracefully...",
            EOL
        );
        shutdown_clone.notify_waiters();
    });

    // Handle SIGTERM (systemd, Docker)
    #[cfg(unix)]
    {
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            let mut sigterm = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            )
                .context("Failed to listen for SIGTERM")?;
            sigterm.recv().await;
            eprintln!(
                "{}SIGTERM received, shutting down gracefully...",
                EOL
            );
            shutdown_clone.notify_waiters();
            Ok::<(), anyhow::Error>(())
        });
    }

    // Handle Ctrl+Break (console uses it) on Windows
    #[cfg(windows)]
    {
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut ctrl_break) =
                tokio::signal::windows::ctrl_break()
            {
                ctrl_break.recv().await;
                eprintln!(
                    "{}Ctrl+Break received, shutting down gracefully...",
                    EOL
                );
                shutdown_clone.notify_waiters();
            }
        });
    }

    loop {
        tokio::select! {
            _=shutdown.notified()=>break,
            result=listener.accept()=>{
                match result{
                    Ok((socket,client_addr))=>{
                        let upstream_port=args.upstream_port;
                        tokio::spawn(async move{
                            if let Err(e)=handle_client_connection(socket,client_addr,upstream_port).await{
                                eprintln!("Client handler error from {}: {:#}",client_addr,e);
                            }
                        });
                    }
                    Err(e)=>{
                        eprintln!("Accept error: {}",e);
                    }
                }
            }
        }
    }

    eprintln!("Stopped accepting connections.");
    Ok(())
}
