use clap::{Parser, ValueEnum};
use http_body_util::BodyExt;
use hyper::{body::Incoming, server::conn::http1, service::service_fn, Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use log::{debug, error, info, warn};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;

mod socks5;
use socks5::Socks5Client;

#[derive(Parser, Debug)]
#[command(name = "karako_http_socks5h_proxy")]
#[command(version = "0.1.0")]
#[command(author = "Your Name <your.email@example.com>")]
#[command(about = "HTTP proxy that forwards requests through a SOCKS5 upstream server")]
#[command(long_about = "This is a simple HTTP proxy that:\n  \
    - Listens on a local HTTP port\n  \
    - Accepts HTTP CONNECT and request forwarding\n  \
    - Tunnels all traffic through a SOCKS5 upstream server\n  \
    - Preserves all request headers and body content")]
struct Args {
    #[arg(short = 'p', long, default_value = "12345")]
    listen_port: u16,

    #[arg(short = 'h', long, default_value = "127.0.0.1")]
    upstream_host: String,

    #[arg(short = 'u', long, default_value = "9050")]
    upstream_port: u16,

    #[arg(short = 'l', long, default_value = "info")]
    log_level: LogLevelValueEnum,

    #[arg(short = 'v', long)]
    verbose: bool,

    #[arg(short = 'D', long)]
    debug: bool,

    #[arg(long)]
    log_bodies: bool,

    #[arg(long, default_value = "1024")]
    log_body_limit: usize,

    #[arg(long)]
    no_banner: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum LogLevelValueEnum {
    Off,
    Error,
    Warn,
    Info,
    Debug,
    Trace,
}

impl LogLevelValueEnum {
    fn to_log_level(&self) -> log::LevelFilter {
        match self {
            LogLevelValueEnum::Off => log::LevelFilter::Off,
            LogLevelValueEnum::Error => log::LevelFilter::Error,
            LogLevelValueEnum::Warn => log::LevelFilter::Warn,
            LogLevelValueEnum::Info => log::LevelFilter::Info,
            LogLevelValueEnum::Debug => log::LevelFilter::Debug,
            LogLevelValueEnum::Trace => log::LevelFilter::Trace,
        }
    }
}

#[derive(Clone)]
struct ProxyConfig {
    upstream_host: String,
    upstream_port: u16,
    log_bodies: bool,
    log_body_limit: usize,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let log_level = if args.debug {
        log::LevelFilter::Trace
    } else if args.verbose {
        log::LevelFilter::Debug
    } else {
        args.log_level.to_log_level()
    };

    env_logger::Builder::from_default_env()
        .filter_level(log_level)
        .format_timestamp_millis()
        .try_init()
        .ok();

    if !args.no_banner {
        eprintln!("╔═══════════════════════════════════════════════════════════╗");
        eprintln!("║    karako_http_socks5h_proxy v0.1.0                      ║");
        eprintln!("║    HTTP-to-SOCKS5H Proxy                                 ║");
        eprintln!("╚═══════════════════════════════════════════════════════════╝");
        eprintln!();
    }

    info!(
        "Starting proxy with configuration: listen=127.0.0.1:{}, upstream={}:{}",
        args.listen_port, args.upstream_host, args.upstream_port
    );

    if args.verbose {
        debug!("Verbose mode enabled");
    }

    if args.debug {
        debug!("Debug mode enabled");
    }

    if args.log_bodies {
        debug!(
            "Request/response body logging enabled (limit: {} bytes)",
            args.log_body_limit
        );
    }

    let config = ProxyConfig {
        upstream_host: args.upstream_host.clone(),
        upstream_port: args.upstream_port,
        log_bodies: args.log_bodies,
        log_body_limit: args.log_body_limit,
    };

    let config = Arc::new(config);
    let addr = SocketAddr::from(([127, 0, 0, 1], args.listen_port));
    let listener = TcpListener::bind(addr).await?;

    info!("✓ Proxy listening on http://{}", addr);
    eprintln!("Proxy listening on http://{}", addr);
    eprintln!(
        "Forwarding through SOCKS5 upstream {}:{}",
        args.upstream_host, args.upstream_port
    );
    eprintln!("Log level: {:?}", log_level);
    eprintln!();

    loop {
        let (socket, _) = listener.accept().await?;
        let io = TokioIo::new(socket);
        let config = Arc::clone(&config);

        tokio::spawn(async move {
            let service = service_fn(move |req| {
                let config = Arc::clone(&config);
                handle_request(req, config)
            });

            if let Err(err) = http1::Builder::new().serve_connection(io, service).await {
                error!("Connection error: {}", err);
            }
        });
    }
}

fn extract_host_port(req: &Request<Incoming>) -> (String, u16) {
    debug!("Extracting target host and port from request");

    if let Some(host_header) = req.headers().get("host") {
        if let Ok(host_str) = host_header.to_str() {
            debug!("Found Host header: {}", host_str);

            if let Some((host, port_str)) = host_str.rsplit_once(':') {
                if let Ok(port) = port_str.parse::<u16>() {
                    debug!("Extracted host={}, port={}", host, port);
                    return (host.to_string(), port);
                }
            }

            debug!("Using Host header without explicit port (defaulting to 80)");
            return (host_str.to_string(), 80);
        }
    }

    let uri = req.uri();
    debug!("Extracting from URI: {}", uri);

    let host = uri.host().unwrap_or("127.0.0.1");
    let port = uri.port_u16().unwrap_or(80);

    debug!("Extracted from URI: host={}, port={}", host, port);

    (host.to_string(), port)
}

async fn handle_request(
    req: Request<Incoming>,
    config: Arc<ProxyConfig>,
) -> Result<Response<String>, Box<dyn std::error::Error + Send + Sync>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let version = req.version();

    info!("→ {} {} (HTTP/{:?})", method, uri, version);

    let headers = req.headers().clone();
    let (host, port) = extract_host_port(&req);

    debug!("Target destination: {}:{}", host, port);

    let body = req.into_body();
    let body_bytes = body.collect().await?.to_bytes();

    if config.log_bodies && !body_bytes.is_empty() {
        let body_str = String::from_utf8_lossy(&body_bytes);
        let truncated = if body_bytes.len() > config.log_body_limit {
            format!(
                "{}... (truncated, total: {} bytes)",
                &body_str[..config.log_body_limit.min(body_bytes.len())],
                body_bytes.len()
            )
        } else {
            body_str.to_string()
        };
        debug!("Request body: {}", truncated);
    } else if !body_bytes.is_empty() {
        debug!(
            "Request body size: {} bytes (use --log-bodies to see content)",
            body_bytes.len()
        );
    }

    debug!(
        "Connecting to SOCKS5 upstream {}:{} for target {}:{}",
        config.upstream_host, config.upstream_port, host, port
    );

    let upstream_ip: std::net::IpAddr = match config.upstream_host.parse() {
        Ok(ip) => ip,
        Err(_) => {
            error!("Invalid upstream host IP: {}", config.upstream_host);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body("Invalid SOCKS5 server address".to_string())?);
        }
    };

    let socks_client = match Socks5Client::new(upstream_ip, config.upstream_port).await {
        Ok(client) => {
            info!("✓ SOCKS5 connection established");
            client
        }
        Err(e) => {
            error!("SOCKS5 connection failed: {:?}", e);
            return Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(format!("SOCKS5 connection failed: {:?}", e))?);
        }
    };

    let request_line = format!(
        "{} {} HTTP/1.1\r\n",
        method,
        uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/")
    );

    debug!("Sending request line: {}", request_line.trim());

    let mut request = request_line;

    for (name, value) in headers.iter() {
        let value_str = match value.to_str() {
            Ok(s) => s,
            Err(_) => {
                warn!("Skipping header with invalid UTF-8: {:?}", name);
                continue;
            }
        };

        debug!("Forwarding header: {}: {}", name, value_str);
        request.push_str(&format!("{}: {}\r\n", name, value_str));
    }

    request.push_str(&format!("Content-Length: {}\r\n", body_bytes.len()));
    request.push_str("\r\n");

    let request_bytes = bytes::Bytes::from(request.into_bytes());

    match socks_client.forward_request(&host, port, request_bytes).await {
        Ok(response_bytes) => {
            if config.log_bodies && !response_bytes.is_empty() {
                let response_str = String::from_utf8_lossy(&response_bytes);
                let truncated = if response_bytes.len() > config.log_body_limit {
                    format!(
                        "{}... (truncated, total: {} bytes)",
                        &response_str[..config.log_body_limit.min(response_bytes.len())],
                        response_bytes.len()
                    )
                } else {
                    response_str.to_string()
                };
                debug!("Response data: {}", truncated);
            } else if !response_bytes.is_empty() {
                debug!(
                    "Response size: {} bytes (use --log-bodies to see content)",
                    response_bytes.len()
                );
            }

            info!("✓ Response forwarded ({} bytes)", response_bytes.len());

            Ok(Response::new(
                String::from_utf8_lossy(&response_bytes).to_string(),
            ))
        }
        Err(e) => {
            error!("Failed to forward request: {:?}", e);
            Ok(Response::builder()
                .status(StatusCode::BAD_GATEWAY)
                .body(format!("Failed to forward request: {:?}", e))?)
        }
    }
}
