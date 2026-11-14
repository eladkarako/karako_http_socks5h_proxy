//! Per-request handling for HTTP/1.x clients.
//!
//! The connection-level work is done by `hyper-util`'s auto server, so
//! this module only sees a parsed request. It logs what the client asked
//! for (without the query string), then routes `CONNECT` to the tunnel
//! module and everything else to the forwarder.

use http::{Request, Response};
use hyper::body::{Body, Incoming};

use crate::proxy_http_forwarder;
use crate::proxy_http_forwarder::{
    build_forward_request, extract_target, BoxBody, ForwardingOptions,
};
use crate::socks5_handshake_flow::Socks5Auth;
use crate::upstream_http_client::UpstreamClient;

/// Handles one HTTP/1.x request on an established client connection.
///
/// Returns a response or an error that the service layer maps to a
/// status code; the body is always streamed, never collected.
pub(crate) async fn handle_http1_request(
    req: Request<Incoming>,
    upstream_addr: std::net::SocketAddr,
    client_addr: std::net::SocketAddr,
    client: UpstreamClient,
    auth: Socks5Auth,
    forwarding: ForwardingOptions,
) -> anyhow::Result<Response<BoxBody>> {
    let started = std::time::Instant::now();
    let method = req.method().clone();
    let uri = req.uri().clone();
    let version = req.version();
    let header_count = req.headers().len();
    let target = extract_target(&req)?;
    let (host, port) = (target.host, target.port);
    log_info!(
        "[HTTP/1.1] {} {} (host: {}:{}) from {}",
        method,
        proxy_http_forwarder::loggable_target(&uri),
        host,
        port,
        client_addr
    );
    log_debug!(
        "[HTTP/1.1] {} header(s), body size hint: {:?}",
        header_count,
        req.body().size_hint()
    );
    let response = match method.as_str() {
        // CONNECT already carries its authority in the request target,
        // so the normal URI/Host rewriting does not apply.
        "CONNECT" => {
            crate::http_connect_tunnel::connect_through_socks5(
                req,
                host,
                port,
                upstream_addr,
                auth,
                forwarding,
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
                client_addr,
                forwarding,
            )?;
            proxy_http_forwarder::send_http_request(
                &client,
                forward_req,
                version,
                forwarding,
            )
                .await
        }
    };
    match &response {
        Ok(response) => log_debug!(
            "[HTTP/1.1] {} {} -> {} in {:?}",
            method,
            proxy_http_forwarder::loggable_target(&uri),
            response.status(),
            started.elapsed()
        ),
        Err(error) => log_debug!(
            "[HTTP/1.1] {} {} failed in {:?}: {:#}",
            method,
            proxy_http_forwarder::loggable_target(&uri),
            started.elapsed(),
            error
        ),
    }
    response
}
