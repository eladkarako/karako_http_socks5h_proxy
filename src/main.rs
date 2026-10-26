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

/// Handle HTTP/2 request
async fn handle_h2_request(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<Full<Bytes>>> {
    let method = req.method().clone();
    let uri = req.uri().clone();

    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);

    eprintln!("{} {} (host: {}:{})", method, uri, host, port);

    // HTTP/2 doesn't support CONNECT tunneling in the traditional sense
    if method == hyper::http::Method::CONNECT {
        return Ok(Response::builder()
            .status(StatusCode::METHOD_NOT_ALLOWED)
            .body(Full::new(Bytes::from(
                "CONNECT not supported; use HTTP/2 directly",
            )))
            .unwrap());
    }

    let body_bytes = read_body(&mut req).await?;

    // Build forward request
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
        .version(hyper::Version::HTTP_2);

    // Copy headers (skip hop-by-hop and connection-specific headers)
    for (name, value) in req.headers().iter() {
        let name_str = name.as_str().to_lowercase();
        if !matches!(
            name_str.as_str(),
            "host"
                | "connection"
                | "te"
                | "trailer"
                | "transfer-encoding"
                | "upgrade"
        ) {
            forward_req = forward_req.header(name, value);
        }
    }

    // Set authority header
    forward_req =
        forward_req.header("host", format!("{}:{}", host, port));

    // Add content-length if body present
    if !body_bytes.is_empty() {
        forward_req =
            forward_req.header("content-length", body_bytes.len());
    }

    let forward_req = forward_req
        .body(Full::new(Bytes::from(body_bytes)))
        .context("Failed to build forward request")?;

    // Connect through SOCKS5 tunnel
    let mut upstream_stream =
        connect_to_socks5(upstream_port).await?;
    socks5_handshake(&mut upstream_stream, &host, port).await?;

    // Establish HTTP/2 connection
    let io = hyper_util::rt::TokioIo::new(upstream_stream);
    let (mut sender, conn) =
        hyper::client::conn::http2::Builder::new(
            hyper_util::rt::TokioExecutor::new(),
        )
            .handshake(io)
            .await
            .context(
                "Failed to establish HTTP/2 connection through SOCKS5",
            )?;

    // Spawn connection maintenance task
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Upstream HTTP/2 connection error: {}", e);
        }
    });

    // Send request and collect response
    let response = sender
        .send_request(forward_req)
        .await
        .context("Failed to send HTTP/2 request through SOCKS5")?;

    let (parts, body) = response.into_parts();
    let body_bytes = http_body_util::BodyExt::collect(body)
        .await
        .context("Failed to read HTTP/2 response body")?
        .to_bytes();

    // Filter response headers to remove HTTP/1.1-specific headers incompatible with HTTP/2
    let mut response_builder =
        Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str().to_lowercase();
        // Skip HTTP/1.1-only headers
        if !matches!(
            name_str.as_str(),
            "connection"
                | "keep-alive"
                | "transfer-encoding"
                | "upgrade"
                | "proxy-connection"
                | "te"
                | "trailer"
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
    timeout(
        Duration::from_secs(30),
        upstream_stream.write_all(SOCKS5_GREETING),
    )
        .await
        .context("Failed to send SOCKS5 greeting")??;
    eprintln!("Sent SOCKS5 greeting: {:?}", SOCKS5_GREETING);

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

    ensure!(
        host.len() <= MAX_HOSTNAME_LEN,
        "Hostname too long: {} bytes (max {})",
        host.len(),
        MAX_HOSTNAME_LEN
    );

    let mut connect_req = [0u8; SOCKS5_CONNECT_MAX_LEN];
    let len = 6 + host.len() + 2;

    connect_req[0] = 5;
    connect_req[1] = 1;
    connect_req[2] = 0;
    connect_req[3] = 3;
    connect_req[4] = host.len() as u8;
    connect_req[5..5 + host.len()].copy_from_slice(host.as_bytes());
    connect_req[5 + host.len()] = (port >> 8) as u8;
    connect_req[5 + host.len() + 1] = (port & 0xff) as u8;

    eprintln!(
        "Sending SOCKS5 CONNECT for {}:{}: {:?}",
        host,
        port,
        &connect_req[..len]
    );
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

    match resp_header[3] {
        1 => {
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
                            if let Err(e) = handle_h2_client_connection(socket, addr, upstream_port).
                        await {
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

/// Handle HTTP/2 client connection
async fn handle_h2_client_connection(
    socket: TcpStream,
    _addr: std::net::SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = Socks5Http2ProxyService { upstream_port };
    let builder = hyper::server::conn::http2::Builder::new(
        hyper_util::rt::TokioExecutor::new(),
    );
    builder.serve_connection(io, service).await?;
    Ok(())
}
