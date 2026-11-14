//! Per-request handling for HTTP/2 clients.
//!
//! HTTP/2 runs over the same `hyper-util` auto server as HTTP/1.x; this
//! module only differs in its log label. `CONNECT` uses the extended
//! protocol enabled in the service builder, while ordinary requests take
//! the same forwarding path as HTTP/1.1.

use http::{Request, Response};
use hyper::body::{Body, Incoming};

use crate::proxy_http_forwarder;
use crate::proxy_http_forwarder::{
    build_forward_request, extract_target, BoxBody, ForwardingOptions,
};
use crate::socks5_handshake_flow::Socks5Auth;
use crate::upstream_http_client::UpstreamClient;

/// Handles one HTTP/2 stream.
///
/// Each stream is independent, which is why the service caps concurrent
/// streams: every forwarded request opens its own upstream SOCKS5
/// connection.
pub(crate) async fn handle_h2_request(
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
        "[HTTP/2] {} {} (host: {}:{}) from {}",
        method,
        proxy_http_forwarder::loggable_target(&uri),
        host,
        port,
        client_addr
    );
    log_debug!(
        "[HTTP/2] {} header(s), body size hint: {:?}",
        header_count,
        req.body().size_hint()
    );
    let response = match method.as_str() {
        // HTTP/2 CONNECT carries `:authority` instead of a `Host`
        // header; `extract_target` has already normalized both forms.
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
            "[HTTP/2] {} {} -> {} in {:?}",
            method,
            proxy_http_forwarder::loggable_target(&uri),
            response.status(),
            started.elapsed()
        ),
        Err(error) => log_debug!(
            "[HTTP/2] {} {} failed in {:?}: {:#}",
            method,
            proxy_http_forwarder::loggable_target(&uri),
            started.elapsed(),
            error
        ),
    }
    response
}
