use http::{Request, Response};
use hyper::body::{Body, Incoming};

use crate::http::{
    self, build_forward_request, extract_target, BoxBody,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
};

pub(crate) async fn handle_http1_request(
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
        "[HTTP/1.1] {} {} (host: {}:{}) from {}",
        method, uri, host, port, client_addr
    );
    log_debug!(
        "[HTTP/1.1] {} header(s), body size hint: {:?}",
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
                false,
            )
                .await
        }
        _ => {
            let forward_req = build_forward_request(
                &method,
                &uri,
                &host,
                port,
                req,
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
            http::send_http_request(
                upstream_stream_clone,
                forward_req,
                &method,
            )
                .await
        }
    };
    match &response {
        Ok(response) => log_debug!(
            "[HTTP/1.1] {} {} -> {} in {:?}",
            method,
            uri,
            response.status(),
            started.elapsed()
        ),
        Err(error) => log_debug!(
            "[HTTP/1.1] {} {} failed in {:?}: {:#}",
            method,
            uri,
            started.elapsed(),
            error
        ),
    }
    response
}
