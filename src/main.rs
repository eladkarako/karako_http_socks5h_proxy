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
use hyper::service::Service;
use std::convert::Infallible;
use std::pin::Pin;
use std::sync::Arc;
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

/// HTTP/2-only proxy service via SOCKS5h
#[derive(Clone)]
struct Socks5Http2ProxyService {
    upstream_port: u16,
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
        // <- Change &mut to &
        let upstream_port = self.upstream_port;
        Box::pin(async move {
            match handle_h2_request(req, upstream_port).await {
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
async fn handle_h2_request(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!("{} {} (host: {}:{})", method, uri, host, port);

    if method == hyper::http::Method::CONNECT {
        eprintln!("[CONNECT] Tunneling to {}:{}", host, port);

        // Establish SOCKS5 connection to target
        let mut upstream_stream = connect_to_socks5(upstream_port).await?;
        socks5_handshake(&mut upstream_stream, &host, port).await?;

        // Return 200 to signal tunnel ready
        // (Real tunnel happens via socket upgrade, which hyper doesn't support directly)
        return Ok(Response::builder()
            .status(StatusCode::OK)
            .body(Full::new(Bytes::new()))
            .unwrap());
    }

    // Regular HTTP request handling (your existing code)
    let body_bytes = read_body(&mut req).await?;
    let mut forward_req = Request::builder()
        .method(method.clone())
        .uri(format!(
            "http://{}:{}{}",
            host,
            port,
            uri.path_and_query()
                .map(|pq| pq.to_string())
                .unwrap_or_else(|| "/".to_string())
        ))
        .version(hyper::Version::HTTP_11);

    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if !matches!(
            name_str.as_str(),
            "host" | "connection" | "te" | "trailer" | "transfer-encoding" | "upgrade"
        ) {
            forward_req = forward_req.header(name, value);
        }
    }

    forward_req = forward_req.header("host", format!("{}:{}", host, port));
    if !body_bytes.is_empty() {
        forward_req = forward_req.header("content-length", body_bytes.len());
    }

    let forward_req = forward_req
        .body(Full::new(Bytes::from(body_bytes)))
        .context("Failed to build forward request")?;

    let mut upstream_stream = connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, &host, port).await?;

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

    let mut response_builder = Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str().to_lowercase();
        if !matches!(
            name_str.as_str(),
            "connection" | "keep-alive" | "transfer-encoding" | "upgrade" | "proxy-connection" | "te" | "trailer"
        ) {
            response_builder = response_builder.header(name, value);
        }
    }

    Ok(response_builder
        .body(Full::new(body_bytes))
        .context("Failed to build response")?)
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
    eprintln!("[SOCKS5] Starting handshake for {}:{}", host, port);

    // Greeting
    timeout(
        Duration::from_secs(30),
        upstream_stream.write_all(SOCKS5_GREETING),
    )
        .await
        .context("SOCKS5 greeting write timeout")?
        .context("Failed to send SOCKS5 greeting")?;
    eprintln!("[SOCKS5] Sent greeting: {:?}", SOCKS5_GREETING);

    let mut resp = [0u8; 2];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp),
    )
        .await
        .context("SOCKS5 greeting read timeout")?
        .context("Failed to read SOCKS5 greeting response")?;
    eprintln!(
        "[SOCKS5] Greeting response: {:?} (ver={}, method={})",
        resp, resp[0], resp[1]
    );

    ensure!(
        resp[0] == 5,
        "Invalid SOCKS5 version: {} (expected 5)",
        resp[0]
    );
    ensure!(resp[1] != 0xff, "SOCKS5 server rejected all methods");

    // Connect request
    ensure!(
        host.len() <= MAX_HOSTNAME_LEN,
        "Hostname too long: {} bytes",
        host.len()
    );

    let mut connect_req = [0u8; SOCKS5_CONNECT_MAX_LEN];
    let len = 6 + host.len() + 2;
    connect_req[0] = 5;
    connect_req[1] = 1; // CONNECT
    connect_req[2] = 0; // Reserved
    connect_req[3] = 3; // Domain name
    connect_req[4] = host.len() as u8;
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes());
    connect_req[5 + host.len()] = (port >> 8) as u8;
    connect_req[5 + host.len() + 1] = (port & 0xff) as u8;

    eprintln!(
        "[SOCKS5] Sending CONNECT request ({} bytes) for {}:{}",
        len, host, port
    );
    timeout(
        Duration::from_secs(30),
        upstream_stream.write_all(&connect_req[..len]),
    )
        .await
        .context("SOCKS5 CONNECT write timeout")?
        .context("Failed to send SOCKS5 CONNECT")?;

    let mut resp_header = [0u8; 4];
    timeout(
        Duration::from_secs(30),
        upstream_stream.read_exact(&mut resp_header),
    )
        .await
        .context("SOCKS5 CONNECT response read timeout")?
        .context("Failed to read SOCKS5 CONNECT response")?;

    eprintln!(
        "[SOCKS5] CONNECT response: ver={}, rep={}, rsv={}, atyp={}",
        resp_header[0],
        resp_header[1],
        resp_header[2],
        resp_header[3]
    );

    ensure!(
        resp_header[0] == 5,
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

    // Read address field based on type
    match resp_header[3] {
        1 => {
            let mut addr_port = [0u8; 6];
            timeout(
                Duration::from_secs(10),
                upstream_stream.read_exact(&mut addr_port),
            )
                .await
                .context("SOCKS5 IPv4 response read timeout")?
                .context("Failed to read SOCKS5 IPv4 response")?;
            eprintln!("[SOCKS5] IPv4 response: {:?}", addr_port);
        }
        3 => {
            let mut len_byte = [0u8; 1];
            upstream_stream
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
            upstream_stream
                .read_exact(&mut domain_and_port)
                .await
                .context("Failed to read SOCKS5 domain response")?;
            eprintln!(
                "[SOCKS5] Domain response: {:?}",
                domain_and_port
            );
        }
        4 => {
            let mut addr_port = [0u8; 18];
            upstream_stream
                .read_exact(&mut addr_port)
                .await
                .context("Failed to read SOCKS5 IPv6 response")?;
            eprintln!("[SOCKS5] IPv6 response: {:?}", addr_port);
        }
        atype => {
            bail!("Unsupported address type: {}", atype);
        }
    }

    eprintln!("[SOCKS5] Handshake complete for {}:{}", host, port);
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
                "Failed to bind HTTP/2 listener on 127.0.0.1:{}",
                args.http_port
            )
        })?;

    eprintln!(
        "HTTP/2 proxy listening on 127.0.0.1:{}",
        args.http_port
    );
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        args.upstream_port
    );

    let shutdown = Arc::new(tokio::sync::Notify::new());

    // Ctrl+C handler
    let shutdown_clone = shutdown.clone();
    let _signal_handle = tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!("\nCtrl+C received, shutting down gracefully...");
        shutdown_clone.notify_waiters();
    });
    // 'q' key handler
    let shutdown_clone = shutdown.clone();
    let _stdin_handle = tokio::spawn(async move {
        use crossterm::{event, terminal};
        use std::time::Duration;

        let _ = terminal::enable_raw_mode();

        loop {
            if event::poll(Duration::from_millis(100)).unwrap_or(false) {
                if let Ok(event::Event::Key(key_event)) = event::read() {
                    if matches!(key_event.code, event::KeyCode::Char('q') | event::KeyCode::Char('Q')) {
                        let _ = terminal::disable_raw_mode();
                        eprintln!("\n'q' pressed, shutting down gracefully...");
                        shutdown_clone.notify_waiters();
                        break;
                    }
                }
            }
        }
    });


    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            result = listener.accept() => {
                match result {
                    Ok((socket, addr)) => {
                        let upstream_port = args.upstream_port;
                        tokio::spawn(async move {
                            if let Err(e) = handle_h2_client_connection(socket, addr, upstream_port).await {
                                eprintln!("Client handler error from {}: {:#}", addr, e);
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
async fn handle_h2_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    eprintln!("[CLIENT] New connection from {}", addr);
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = Socks5Http2ProxyService { upstream_port };

    let builder = hyper::server::conn::http1::Builder::new();

    match builder.serve_connection(io, service).await {
        Ok(_) => {
            eprintln!("[CLIENT] Connection {} closed normally", addr);
            Ok(())
        }
        Err(e) => {
            // Ignore "connection closed" errors
            if !e.to_string().contains("closed") {
                eprintln!("[CLIENT] Connection {} error: {}", addr, e);
            }
            Ok(())
        }
    }
}
