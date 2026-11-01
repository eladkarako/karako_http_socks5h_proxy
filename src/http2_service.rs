use anyhow::bail;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::Full;
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

#[derive(Clone)]
pub struct Socks5Http2ProxyService {
    pub upstream_port: u16,
    pub client_addr: std::net::SocketAddr,
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

pub async fn handle_http2_client_connection(
    socket: TcpStream,
    addr: std::net::SocketAddr,
    upstream_port: u16,
) -> anyhow::Result<()> {
    eprintln!("[HTTP/2 CLIENT] New connection from {}", addr);
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = Socks5Http2ProxyService {
        upstream_port,
        client_addr: addr,
    };
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

async fn handle_h2_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> anyhow::Result<Response<Full<Bytes>>> {
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
) -> anyhow::Result<Response<Full<Bytes>>> {
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
