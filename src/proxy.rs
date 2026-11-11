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

use crate::http::{full_body, BadRequestError, BoxBody, StatusCode};
use crate::http1_service::handle_http1_request;
use crate::http2_service::handle_h2_request;
use crate::socks5::Socks5Auth;
use crate::upstream::UpstreamClient;

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

#[derive(Clone)]
struct ProxyService {
    upstream_addr: SocketAddr,
    client_addr: SocketAddr,
    client: UpstreamClient,
    auth: Socks5Auth,
}

impl Service<Request<Incoming>> for ProxyService {
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<
        Box<
            dyn Future<Output = Result<Self::Response, Self::Error>>
                + Send
                + 'static,
        >,
    >;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let upstream_addr = self.upstream_addr;
        let client_addr = self.client_addr;
        let client = self.client.clone();
        let auth = self.auth.clone();
        Box::pin(async move {
            let result = match req.version() {
                hyper::Version::HTTP_2 => {
                    handle_h2_request(
                        req,
                        upstream_addr,
                        client_addr,
                        client,
                        auth,
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
                    )
                        .await
                }
            };
            Ok(result.unwrap_or_else(|e| {
                log_error!(
                    "Request handling error from {}: {:#}",
                    client_addr, e
                );
                error_response(&e)
            }))
        })
    }
}

fn error_response(error: &anyhow::Error) -> Response<BoxBody> {
    let (status, message) =
        match error.downcast_ref::<BadRequestError>() {
            Some(bad_request) => {
                (StatusCode::BAD_REQUEST, bad_request.0.clone())
            }
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

pub async fn handle_client_connection(
    socket: TcpStream,
    addr: SocketAddr,
    upstream_addr: SocketAddr,
    client: UpstreamClient,
    auth: Socks5Auth,
) -> Result<()> {
    let started = std::time::Instant::now();
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = ProxyService {
        upstream_addr,
        client_addr: addr,
        client,
        auth,
    };
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(
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
