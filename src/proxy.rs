use crate::http::{
    self, build_forward_request, extract_host, extract_port,
    read_request_body,
};
use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
};
use anyhow::{bail, Context, Result};
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::Service;
use std::convert::Infallible;
use std::pin::Pin;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;
use tokio::time::timeout;
type BoxBody =
http_body_util::combinators::BoxBody<Bytes, Infallible>;
const TUNNEL_TIMEOUT: Duration = Duration::from_secs(600);

pub async fn handle_client_connection(
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
    configure_tokio_socket(
        &upstream_stream,
        SocketConfiguration::SOCKS_HTTP_REQUEST,
    )?;
    let response = http::send_http_request(
        upstream_stream,
        forward_req,
        &method,
    )
        .await?;
    Ok(response)
}

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
            let target_stream =
                connect_to_socks5_server(upstream_port).await?;
            let mut target_stream_clone = target_stream;
            establish_socks5_connection(
                &mut target_stream_clone,
                &host,
                port,
            )
                .await?;
            configure_tokio_socket(
                &target_stream_clone,
                SocketConfiguration::SOCKS_TUNNEL,
            )?;
            eprintln!(
                "[CONNECT TUNNEL] SOCKS5 connection established, sending 200 OK"
            );
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("Connection", "Upgrade")
                .body(BoxBody::new(Full::new(Bytes::new()).boxed()))
                .context("Failed to build CONNECT response")?;
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
            let response = http::send_http_request(
                upstream_stream_clone,
                forward_req,
                &method,
            )
                .await?;
            let (parts, body) = response.into_parts();
            let body_bytes = if matches!(method.as_str(), "HEAD")
                || matches!(
                    parts.status,
                    StatusCode::NO_CONTENT
                        | StatusCode::NOT_MODIFIED
                ) {
                Bytes::new()
            } else {
                BodyExt::collect(body).await?.to_bytes()
            };
            Ok(Response::from_parts(
                parts,
                BoxBody::new(Full::new(body_bytes).boxed()),
            ))
        }
    }
}

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
    let mut client = hyper_util::rt::TokioIo::new(upgraded);
    let mut target = target;

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
