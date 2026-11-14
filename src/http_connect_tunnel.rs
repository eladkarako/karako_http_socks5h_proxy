use anyhow::{Context, Result};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_io_timeout::TimeoutStream;

use crate::proxy_http_forwarder::{
    empty_body, BoxBody, ForwardingOptions,
};
use crate::tcp_socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5_handshake_flow::{
    connect_to_socks5_server, establish_socks5_connection,
    Socks5Auth,
};

/// How long a tunnel may sit without any traffic in either direction
/// before it is torn down.
pub(crate) const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Handles a `CONNECT` request by opening a SOCKS5 tunnel to the
/// requested authority and upgrading the client connection to a raw
/// byte stream.
pub async fn connect_through_socks5(
    req: Request<Incoming>,
    host: String,
    port: u16,
    upstream_addr: std::net::SocketAddr,
    auth: Socks5Auth,
    forwarding: ForwardingOptions,
) -> Result<Response<BoxBody>> {
    let started = std::time::Instant::now();
    let client_version = req.version();
    log_info!(
        "[CONNECT TUNNEL] Initiating tunnel to {}:{}",
        host,
        port
    );
    let mut target_stream =
        connect_to_socks5_server(upstream_addr).await?;
    establish_socks5_connection(
        &mut target_stream,
        &host,
        port,
        &auth,
    )
        .await?;
    configure_tokio_socket(
        &target_stream,
        SocketConfiguration::SOCKS_TUNNEL,
    );
    log_debug!(
        "[CONNECT TUNNEL] SOCKS5 connection established in {:?}, sending 200 OK",
        started.elapsed()
    );
    // A 2xx response to CONNECT switches both HTTP/1.1 and HTTP/2 to a
    // raw tunnel, so no upgrade headers are needed (and hop-by-hop
    // headers must not be sent at all).
    let mut response_builder =
        Response::builder().status(StatusCode::OK);
    if forwarding.add_via {
        response_builder = response_builder.header(
            http::header::VIA,
            crate::proxy_http_forwarder::via_value(client_version),
        );
    }
    let response = response_builder
        .body(empty_body())
        .context("Failed to build CONNECT response")?;
    tokio::spawn(async move {
        match hyper::upgrade::on(req).await {
            Ok(upgraded) => {
                log_debug!(
                    "[CONNECT TUNNEL] Upgrade successful for {}:{}",
                    host,
                    port
                );
                if let Err(e) = handle_tunnel(
                    upgraded,
                    target_stream,
                    &host,
                    port,
                )
                    .await
                {
                    log_error!(
                        "[CONNECT TUNNEL] Tunnel error for {}:{}: {:#}",
                        host,
                        port,
                        e
                    );
                }
            }
            Err(e) => {
                log_warn!(
                    "[CONNECT TUNNEL] Upgrade failed for {}:{}: {}",
                    host,
                    port,
                    e
                );
            }
        }
    });
    Ok(response)
}

pub async fn handle_tunnel(
    upgraded: hyper::upgrade::Upgraded,
    target: TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    let started = std::time::Instant::now();
    log_debug!(
        "[TUNNEL] Starting bidirectional tunnel for {}:{}",
        host,
        port
    );
    let mut client = TimeoutStream::new(
        hyper_util::rt::TokioIo::new(upgraded),
    );
    client.set_read_timeout(Some(TUNNEL_IDLE_TIMEOUT));
    client.set_write_timeout(Some(TUNNEL_IDLE_TIMEOUT));
    let mut client = Box::pin(client);

    let mut target = TimeoutStream::new(target);
    target.set_read_timeout(Some(TUNNEL_IDLE_TIMEOUT));
    target.set_write_timeout(Some(TUNNEL_IDLE_TIMEOUT));
    let mut target = Box::pin(target);

    let buffer_size = crate::runtime_buffer_tuning::get().tunnel_buffer_size;
    let (client_to_target, target_to_client) =
        tokio::io::copy_bidirectional_with_sizes(
            &mut client,
            &mut target,
            buffer_size,
            buffer_size,
        )
            .await
            .with_context(|| {
                format!(
                    "Tunnel {}:{} failed (idle timeout is {:?})",
                    host, port, TUNNEL_IDLE_TIMEOUT
                )
            })?;
    log_info!(
        "[TUNNEL] Tunnel {}:{} closed normally after {:?}: client→target: {} bytes, target→client: {} bytes",
        host,
        port,
        started.elapsed(),
        client_to_target,
        target_to_client
    );
    Ok(())
}
