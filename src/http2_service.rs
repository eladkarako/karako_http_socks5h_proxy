use anyhow::bail;
use http::{Request, Response};
use http_body_util::BodyExt;
use hyper::body::Incoming;

use crate::http::{
    self, build_forward_request, extract_target, read_request_body,
    BoxBody,
};
use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
};

pub(crate) async fn handle_h2_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> anyhow::Result<Response<BoxBody>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let target = extract_target(&req)?;
    let (host, port) = (target.host, target.port);
    eprintln!(
        "[HTTP/2] {} {} (host: {}:{}) from {}",
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
) -> anyhow::Result<Response<BoxBody>> {
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
    Ok(response.map(|body| body.boxed()))
}
