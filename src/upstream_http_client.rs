//! Pooled HTTP client whose transport is a SOCKS5 connection.
//!
//! `hyper-util`'s legacy client handles pooling, retries and HTTP/1.1
//! framing; the only custom part is [`SocksConnector`], which opens a
//! fresh SOCKS5 connection whenever the pool needs one. Because the
//! connector receives the destination URI, host names still reach the
//! SOCKS5 server unresolved.

use anyhow::Context;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use http::Uri;
use hyper::body::Incoming;
use hyper_util::client::legacy::Client;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;
use tower_service::Service;

use crate::tcp_socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5_handshake_flow::{
    connect_to_socks5_server, establish_socks5_connection,
    Socks5Auth,
};

/// How long an idle upstream connection stays in the pool.
const POOL_IDLE_TIMEOUT: Duration = Duration::from_secs(90);

/// Maximum number of idle connections kept per `host:port`.
const POOL_MAX_IDLE_PER_HOST: usize = 8;

/// Pooled HTTP/1.1 client whose connections are established through the
/// upstream SOCKS5h server.
pub(crate) type UpstreamClient = Client<SocksConnector, Incoming>;

/// Builds the shared upstream client. Cloning the returned value shares
/// the same connection pool.
pub(crate) fn build(
    upstream_addr: std::net::SocketAddr,
    auth: Socks5Auth,
) -> UpstreamClient {
    let mut builder =
        Client::builder(hyper_util::rt::TokioExecutor::new());
    builder
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .retry_canceled_requests(true)
        .build(SocksConnector {
            upstream_addr,
            auth,
        })
}

/// Connects to a destination URI by opening a fresh SOCKS5h tunnel.
///
/// The pooled client calls this only when it has no reusable idle
/// connection for the requested `host:port`.
#[derive(Clone)]
pub(crate) struct SocksConnector {
    /// SOCKS5 server each new connection is opened through.
    upstream_addr: std::net::SocketAddr,
    /// Credentials for that server.
    auth: Socks5Auth,
}

impl Service<Uri> for SocksConnector {
    type Response = TokioIo<TcpStream>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<
        Box<
            dyn Future<Output=Result<Self::Response, Self::Error>>
            + Send,
        >,
    >;

    fn poll_ready(
        &mut self,
        _: &mut TaskContext<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(
        &mut self,
        uri: Uri,
    ) -> Self::Future {
        let upstream_addr = self.upstream_addr;
        let auth = self.auth.clone();
        Box::pin(async move {
            let host = uri
                .host()
                .context("upstream URI has no host")?
                .to_string();
            let port = uri
                .port_u16()
                .unwrap_or_else(|| default_port(&uri));
            let mut stream =
                connect_to_socks5_server(upstream_addr).await?;
            establish_socks5_connection(
                &mut stream,
                &host,
                port,
                &auth,
            )
                .await?;
            configure_tokio_socket(
                &stream,
                SocketConfiguration::SOCKS_HTTP_REQUEST,
            );
            Ok(TokioIo::new(stream))
        })
    }
}

/// TLS is impossible on this leg, so only `http` is meaningful; the
/// fallback keeps explicit and implicit ports consistent.
fn default_port(uri: &Uri) -> u16 {
    match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    }
}
