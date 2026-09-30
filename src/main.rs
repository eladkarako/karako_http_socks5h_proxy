use bytes::Bytes;
use clap::{Parser, ValueEnum};
use http_body_util::{BodyExt, Full};
use hyper::{
    body::Incoming, server::conn::http1, service::service_fn, Request,
    Response, StatusCode,
};
use log::{info, warn};
use std::{
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_socks::tcp::Socks5Stream;

#[derive(Parser, Debug)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(about = env!("CARGO_PKG_DESCRIPTION"))]
#[command(
    long_about = "A lightweight HTTP-to-SOCKS5H proxy that forwards HTTP requests through a SOCKS5 proxy server with remote DNS resolution."
)]
#[command(after_help = "EXAMPLES:\n  \
    # Run proxy on localhost:8080, forwarding to SOCKS5 at 192.168.1.100:1080\n  \
    karako_http_socks5h_proxy -u 192.168.1.100:1080\n  \n  \
    # Run proxy on 0.0.0.0:3128 with debug logging\n  \
    karako_http_socks5h_proxy -l 0.0.0.0 -p 3128 -u 192.168.1.100:1080 --log-level debug\n  \n  \
    # Verify proxy is working\n  \
    curl -x http://127.0.0.1:8080 http://example.com")]
struct Args {
    #[arg(
        short,
        long,
        default_value = "127.0.0.1",
        help = "Address to listen on"
    )]
    listen_addr: String,

    #[arg(
        short,
        long,
        default_value = "8080",
        help = "Port to listen on"
    )]
    listen_port: u16,

    #[arg(
        short,
        long,
        help = "Upstream SOCKS5 proxy address (format: host:port)",
        value_name = "HOST:PORT"
    )]
    upstream_socks5: String,

    #[arg(long, default_value = "info", help = "Logging level")]
    log_level: LogLevelValueEnum,
}

#[derive(Debug, Clone, ValueEnum)]
enum LogLevelValueEnum {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

impl LogLevelValueEnum {
    fn to_level_filter(&self) -> log::LevelFilter {
        match self {
            LogLevelValueEnum::Trace => log::LevelFilter::Trace,
            LogLevelValueEnum::Debug => log::LevelFilter::Debug,
            LogLevelValueEnum::Info => log::LevelFilter::Info,
            LogLevelValueEnum::Warn => log::LevelFilter::Warn,
            LogLevelValueEnum::Error => log::LevelFilter::Error,
        }
    }
}

#[derive(Clone)]
struct ProxyConfig {
    upstream_socks5: SocketAddr,
}

#[tokio::main]
async fn main()
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let args = Args::parse();

    env_logger::Builder::new()
        .filter_level(args.log_level.to_level_filter())
        .try_init()?;

    info!(
        "Starting {} v{}",
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION")
    );

    let (host, port) = parse_upstream(&args.upstream_socks5)?;
    let upstream_addr = SocketAddr::new(host, port);

    let config = ProxyConfig { upstream_socks5: upstream_addr };

    let config = Arc::new(config);

    let listen_addr =
        format!("{}:{}", args.listen_addr, args.listen_port);
    let listener = TcpListener::bind(&listen_addr).await?;

    info!("Listening on {}", listen_addr);
    info!("Upstream SOCKS5: {}:{}", host, port);
    eprintln!("Proxy listening on http://{}", listen_addr);

    loop {
        let (stream, peer_addr) = listener.accept().await?;
        let config = config.clone();

        tokio::task::spawn(async move {
            let io = hyper_util::rt::TokioIo::new(stream);
            let service = service_fn(move |req| {
                let config = config.clone();
                handle_request(req, config)
            });

            if let Err(e) = http1::Builder::new()
                .serve_connection(io, service)
                .await
            {
                warn!(
                    "Error serving connection from {}: {}",
                    peer_addr, e
                );
            }
        });
    }
}

async fn handle_request(
    req: Request<Incoming>,
    config: Arc<ProxyConfig>,
) -> Result<
    Response<Full<Bytes>>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    let (host, port) = extract_host_port(&req)?;

    let method = req.method().clone();
    let uri = req.uri().clone();
    let version = req.version();
    let headers = req.headers().clone();

    // Consume the body
    let body = req.into_body().collect().await?.to_bytes();

    // Connect via SOCKS5
    let socks_stream =
        Socks5Stream::connect(config.upstream_socks5, (host, port))
            .await
            .map_err(|e| {
                format!("SOCKS5 connection failed: {}", e)
            })?;

    let (mut reader, mut writer) =
        tokio::io::split(socks_stream.into_inner());

    // Build and send HTTP request
    let http_request =
        build_http_request(&method, &uri, version, &headers, &body)?;
    writer.write_all(&http_request).await?;
    writer.flush().await?;

    // Read response with timeout
    let response_data = tokio::time::timeout(
        std::time::Duration::from_millis(500),
        read_full_response(&mut reader),
    )
        .await
        .unwrap_or_else(|_| Ok(Vec::new()))?;

    // Parse and return response
    parse_http_response(&response_data)
}

fn extract_host_port(
    req: &Request<Incoming>
) -> Result<(String, u16), Box<dyn std::error::Error + Send + Sync>>
{
    let uri = req.uri();
    let host =
        uri.host().ok_or("No host in request URI")?.to_string();
    let port = uri.port_u16().unwrap_or(80);
    Ok((host, port))
}

fn parse_upstream(
    upstream: &str
) -> Result<(IpAddr, u16), Box<dyn std::error::Error + Send + Sync>>
{
    let parts: Vec<&str> = upstream.split(':').collect();
    if parts.len() != 2 {
        return Err("Upstream must be in format 'host:port'".into());
    }

    // Try parsing as IPv6 address first (handles both IPv4 and IPv6)
    let host = match parts[0].parse::<IpAddr>() {
        Ok(addr) => addr,
        Err(_) => {
            // If not an IP, resolve via DNS
            let addrs = std::net::ToSocketAddrs::to_socket_addrs(
                &format!("{}:0", parts[0]),
            )?;
            addrs
                .into_iter()
                .next()
                .ok_or("Could not resolve upstream host")?
                .ip()
        }
    };

    let port = parts[1].parse::<u16>()?;
    Ok((host, port))
}

fn build_http_request(
    method: &hyper::Method,
    uri: &hyper::Uri,
    version: hyper::Version,
    headers: &hyper::HeaderMap,
    body: &[u8],
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let path =
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
    let mut request =
        format!("{} {} {:?}\r\n", method, path, version);

    for (name, value) in headers {
        request.push_str(&format!(
            "{}: {}\r\n",
            name,
            value.to_str()?
        ));
    }

    request.push_str("\r\n");
    let mut result = request.into_bytes();
    result.extend_from_slice(body);

    Ok(result)
}

async fn read_full_response(
    reader: &mut tokio::io::ReadHalf<tokio::net::TcpStream>
) -> Result<Vec<u8>, Box<dyn std::error::Error + Send + Sync>> {
    let mut response = Vec::new();
    let mut buf = [0u8; 4096];

    loop {
        match reader.read(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => response.extend_from_slice(&buf[..n]),
            Err(e) => return Err(Box::new(e)),
        }
    }

    Ok(response)
}

fn parse_http_response(
    data: &[u8]
) -> Result<
    Response<Full<Bytes>>,
    Box<dyn std::error::Error + Send + Sync>,
> {
    if data.is_empty() {
        return Ok(Response::builder()
            .status(StatusCode::GATEWAY_TIMEOUT)
            .body(Full::new(Bytes::from("Gateway Timeout")))?);
    }

    // Simple response parsing: find headers/body split
    let response_str = String::from_utf8_lossy(data);

    if let Some(split_idx) = response_str.find("\r\n\r\n") {
        let headers_part = &response_str[..split_idx];
        let body_part = &data[split_idx + 4..];

        // Extract status code from first line
        let status_line =
            headers_part.lines().next().unwrap_or("HTTP/1.1 200 OK");
        let status_code = status_line
            .split(' ')
            .nth(1)
            .and_then(|s| s.parse::<u16>().ok())
            .unwrap_or(200);

        Ok(Response::builder()
            .status(StatusCode::from_u16(status_code)?)
            .body(Full::new(Bytes::copy_from_slice(body_part)))?)
    } else {
        Ok(Response::builder()
            .status(StatusCode::BAD_GATEWAY)
            .body(Full::new(Bytes::from("Bad Gateway")))?)
    }
}
