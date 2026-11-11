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

use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
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
pub(crate) fn build(upstream_addr: std::net::SocketAddr) -> UpstreamClient {
    let mut builder = Client::builder(
        hyper_util::rt::TokioExecutor::new(),
    );
    builder
        .pool_idle_timeout(POOL_IDLE_TIMEOUT)
        .pool_max_idle_per_host(POOL_MAX_IDLE_PER_HOST)
        .retry_canceled_requests(true)
        .build(SocksConnector { upstream_addr })
}

/// Connects to a destination URI by opening a fresh SOCKS5h tunnel.
///
/// The pooled client calls this only when it has no reusable idle
/// connection for the requested `host:port`.
#[derive(Clone)]
pub(crate) struct SocksConnector {
    upstream_addr: std::net::SocketAddr,
}

impl Service<Uri> for SocksConnector {
    type Response = TokioIo<TcpStream>;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<
        Box<
            dyn Future<Output = Result<Self::Response, Self::Error>>
                + Send,
        >,
    >;

    fn poll_ready(
        &mut self,
        _: &mut TaskContext<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let upstream_addr = self.upstream_addr;
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
            establish_socks5_connection(&mut stream, &host, port)
                .await?;
            configure_tokio_socket(
                &stream,
                SocketConfiguration::SOCKS_HTTP_REQUEST,
            );
            Ok(TokioIo::new(stream))
        })
    }
}

fn default_port(uri: &Uri) -> u16 {
    match uri.scheme_str() {
        Some("https") => 443,
        _ => 80,
    }
}
