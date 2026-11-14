//! Connection-level HTTP service shared by both protocol versions.
//!
//! `hyper-util`'s auto server detects HTTP/1.1 versus HTTP/2 and calls
//! [`ProxyService`] for each request or stream. The service enforces
//! optional client authentication, dispatches to the version-specific
//! handler, and converts any error into the status code carried by
//! [`ProxyError`].

use anyhow::Result;
use hyper::body::Incoming;
use hyper::service::Service;
use hyper::{Request, Response};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::proxy_http_forwarder::{
    full_body, BoxBody, ForwardingOptions, StatusCode,
};
use crate::http1_request_handler::handle_http1_request;
use crate::http2_request_handler::handle_h2_request;
use crate::inbound_proxy_auth::{challenge, ProxyAuth};
use crate::proxy_error_types::ProxyError;
use crate::response_header_policy::{self, ResponsePolicy};
use crate::socks5_handshake_flow::Socks5Auth;
use crate::upstream_http_client::UpstreamClient;

/// How long a client may take to send request headers.
///
/// Slow-header attacks would otherwise hold a connection slot open
/// indefinitely; the value is generous enough for slow links.
const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

/// Upper bound on concurrently multiplexed HTTP/2 streams per client
/// connection. Each stream opens its own upstream SOCKS5 socket, so this
/// keeps a single connection from exhausting upstream resources.
const H2_MAX_CONCURRENT_STREAMS: u32 = 128;

/// PING interval used to detect dead HTTP/2 peers.
const H2_KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(30);

/// How long to wait for a PING acknowledgement before closing the
/// connection.
const H2_KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(20);

/// Per-connection services and policies.
///
/// Grouping them keeps [`handle_client_connection`] small and lets the
/// accept loop build the value once, then clone it per connection. The
/// upstream client is a pool handle, so cloning is cheap and all clones
/// share the same connections.
#[derive(Clone)]
pub struct ProxyServiceConfig {
    /// SOCKS5 server every forwarded request is sent through.
    pub upstream_addr: SocketAddr,
    /// Pooled upstream HTTP client.
    pub client: UpstreamClient,
    /// Upstream SOCKS5 credentials.
    pub auth: Socks5Auth,
    /// Inbound client authentication policy.
    pub inbound_auth: ProxyAuth,
    /// Which optional headers to add.
    pub forwarding: ForwardingOptions,
    /// Which response rewrites to apply.
    pub response_policy: ResponsePolicy,
}

/// Cloneable request handler for one accepted client connection.
///
/// The upstream client is a pool handle, so cloning is cheap and all
/// clones share the same connections.
#[derive(Clone)]
struct ProxyService {
    /// SOCKS5 server every forwarded request is sent through.
    upstream_addr: SocketAddr,
    /// Peer address, used for logs and `X-Forwarded-For`.
    client_addr: SocketAddr,
    /// Pooled upstream HTTP client.
    client: UpstreamClient,
    /// Upstream SOCKS5 credentials.
    auth: Socks5Auth,
    /// Inbound client authentication policy.
    inbound_auth: ProxyAuth,
    /// Which optional headers to add.
    forwarding: ForwardingOptions,
    /// Which response rewrites to apply.
    response_policy: ResponsePolicy,
}

impl Service<Request<Incoming>> for ProxyService {
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<
        Box<
            dyn Future<Output=Result<Self::Response, Self::Error>>
            + Send
            + 'static,
        >,
    >;

    fn call(
        &self,
        req: Request<Incoming>,
    ) -> Self::Future {
        let upstream_addr = self.upstream_addr;
        let client_addr = self.client_addr;
        let client = self.client.clone();
        let auth = self.auth.clone();
        let inbound_auth = self.inbound_auth.clone();
        let forwarding = self.forwarding;
        let response_policy = self.response_policy;
        Box::pin(async move {
            // Cross-domain mocks only exist when permissive CORS is on.
            // Resolving them before dispatch avoids cloning or moving the
            // request twice.
            let cross_domain = if response_policy.cors_allow_all {
                response_header_policy::cross_domain_mock(
                    req.method(),
                    req.uri(),
                )
            } else {
                None
            };
            let mut response = if !inbound_auth.is_authorized(&req) {
                log_warn!(
                    "[AUTH] Rejecting unauthenticated request from {}",
                    client_addr
                );
                challenge()
            } else if let Some(body) = cross_domain {
                log_info!(
                    "[CORS] Serving synthetic cross-domain policy for {}",
                    req.uri().path()
                );
                response_header_policy::cross_domain_response(body)
            } else {
                let result = match req.version() {
                    hyper::Version::HTTP_2 => {
                        handle_h2_request(
                            req,
                            upstream_addr,
                            client_addr,
                            client,
                            auth,
                            forwarding,
                        )
                            .await
                    }
                    _ => {
                        handle_http1_request(
                            req,
                            upstream_addr,
                            client_addr,
                            client,
                            auth,
                            forwarding,
                        )
                            .await
                    }
                };
                result.unwrap_or_else(|e| {
                    log_error!(
                        "Request handling error from {}: {:#}",
                        client_addr,
                        e
                    );
                    error_response(&e)
                })
            };
            response_policy.apply(&mut response);
            Ok(response)
        })
    }
}

/// Maps a handler error to the response sent to the client.
///
/// `BadRequest` keeps its message because it describes a client mistake;
/// upstream failures use fixed text so internal details stay in the logs.
fn error_response(error: &anyhow::Error) -> Response<BoxBody> {
    let (status, message) =
        match error.downcast_ref::<ProxyError>() {
            Some(ProxyError::BadRequest(message)) => {
                (StatusCode::BAD_REQUEST, message.clone())
            }
            Some(ProxyError::Timeout { .. }) => (
                StatusCode::GATEWAY_TIMEOUT,
                "upstream request timed out".to_string(),
            ),
            None => (
                StatusCode::BAD_GATEWAY,
                "upstream request failed".to_string(),
            ),
        };
    let body = full_body(message);
    Response::builder()
        .status(status)
        .body(body)
        .expect("valid error response")
}

/// Serves one client connection until it closes or errors.
///
/// Protocol detection and connection reuse are delegated to `hyper-util`.
/// Returning `Ok(())` for a connection-level error is deliberate: the
/// error is already logged, and the listener should keep running.
pub async fn handle_client_connection(
    socket: TcpStream,
    addr: SocketAddr,
    config: ProxyServiceConfig,
) -> Result<()> {
    let ProxyServiceConfig {
        upstream_addr,
        client,
        auth,
        inbound_auth,
        forwarding,
        response_policy,
    } = config;
    let started = std::time::Instant::now();
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = ProxyService {
        upstream_addr,
        client_addr: addr,
        client,
        auth,
        inbound_auth,
        forwarding,
        response_policy,
    };
    let mut builder = hyper_util::server::conn::auto::Builder::new(
        hyper_util::rt::TokioExecutor::new(),
    );
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .preserve_header_case(true)
        .title_case_headers(false)
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder
        .http2()
        .timer(hyper_util::rt::TokioTimer::new())
        .enable_connect_protocol()
        .max_concurrent_streams(H2_MAX_CONCURRENT_STREAMS)
        .keep_alive_interval(H2_KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(H2_KEEP_ALIVE_TIMEOUT);
    let result = builder
        .serve_connection_with_upgrades(io, service)
        .await;
    log_debug!(
        "[CLOSE] {} disconnected after {:?}",
        addr,
        started.elapsed()
    );
    match result {
        Ok(()) => Ok(()),
        Err(e) => {
            log_warn!("Connection {} error: {}", addr, e);
            Ok(())
        }
    }
}
