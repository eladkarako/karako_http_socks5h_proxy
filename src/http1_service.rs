use anyhow::Context;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::service::Service;
use std::convert::Infallible;
use std::pin::Pin;
use tokio::net::TcpStream;

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
use crate::tunnel::handle_tunnel;

type BoxBody =
http_body_util::combinators::BoxBody<Bytes, Infallible>;

#[derive(Clone)]
pub struct Socks5Http1ProxyService {
    pub upstream_port: u16,
    pub client_addr: std::net::SocketAddr,
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

pub async fn handle_http1_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> anyhow::Result<()> {
    eprintln!("[HTTP/1.1 CLIENT] New connection from {}", addr);
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = Socks5Http1ProxyService {
        upstream_port,
        client_addr: addr,
    };
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

async fn handle_http1_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> anyhow::Result<Response<BoxBody>> {
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
