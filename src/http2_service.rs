use http::{Request, Response};
use hyper::body::{Body, Incoming};

use crate::http::{
    self, build_forward_request, extract_target, BoxBody,
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
    let started = std::time::Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let header_count = req.headers().len();
    let target = extract_target(&req)?;
    let (host, port) = (target.host, target.port);
    log_info!(
        "[HTTP/2] {} {} (host: {}:{}) from {}",
        method, uri, host, port, client_addr
    );
    log_debug!(
        "[HTTP/2] {} header(s), body size hint: {:?}",
        header_count,
        req.body().size_hint()
    );
    let response = match method.as_str() {
        "CONNECT" => {
            crate::tunnel::connect_through_socks5(
                req,
                host,
                port,
                upstream_port,
            )
                .await
        }
        _ => {
            handle_http_request(req, &host, port, upstream_port)
                .await
        }
    };
    match &response {
        Ok(response) => log_debug!(
            "[HTTP/2] {} {} -> {} in {:?}",
            method,
            uri,
            response.status(),
            started.elapsed()
        ),
        Err(error) => log_debug!(
            "[HTTP/2] {} {} failed in {:?}: {:#}",
            method,
            uri,
            started.elapsed(),
            error
        ),
    }
    response
}

async fn handle_http_request(
    req: Request<Incoming>,
    host: &str,
    port: u16,
    upstream_port: u16,
) -> anyhow::Result<Response<BoxBody>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let forward_req = build_forward_request(
        &method, &uri, host, port, req,
    )?;
    let mut upstream_stream =
        connect_to_socks5_server(upstream_port).await?;
    establish_socks5_connection(&mut upstream_stream, host, port)
        .await?;
    configure_tokio_socket(
        &upstream_stream,
        SocketConfiguration::SOCKS_HTTP_REQUEST,
    );
    http::send_http_request(
        upstream_stream,
        forward_req,
        &method,
    )
        .await
}
