use http::{Request, Response};
use hyper::body::{Body, Incoming};

use crate::http::{
    self, build_forward_request, extract_target, BoxBody,
};
use crate::socks5::Socks5Auth;
use crate::upstream::UpstreamClient;

pub(crate) async fn handle_h2_request(
    req: Request<Incoming>,
    upstream_addr: std::net::SocketAddr,
    client_addr: std::net::SocketAddr,
    client: UpstreamClient,
    auth: Socks5Auth,
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
                upstream_addr,
                auth,
            )
                .await
        }
        _ => {
            let forward_req = build_forward_request(
                &method, &uri, &host, port, req,
            )?;
            http::send_http_request(&client, forward_req, &method)
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
