use anyhow::{Context, Result};
use http::{Request, Response, StatusCode};
use hyper::body::Incoming;
use std::io;
use std::time::Duration;
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::proxy_http::{
    empty_body, BoxBody, ForwardingOptions,
};
use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
    Socks5Auth,
};

/// How long a tunnel may sit without any traffic in either direction
/// before it is torn down.
pub(crate) const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

const TUNNEL_BUFFER_SIZE: usize = 16 * 1024;

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
            crate::proxy_http::via_value(client_version),
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
    let client = hyper_util::rt::TokioIo::new(upgraded);
    let (mut client_read, mut client_write) =
        tokio::io::split(client);
    let (mut target_read, mut target_write) =
        tokio::io::split(target);
    let (client_to_target, target_to_client) = tokio::try_join!(
        copy_with_idle_timeout(
            &mut client_read,
            &mut target_write,
            TUNNEL_IDLE_TIMEOUT,
        ),
        copy_with_idle_timeout(
            &mut target_read,
            &mut client_write,
            TUNNEL_IDLE_TIMEOUT,
        ),
    )
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

async fn copy_with_idle_timeout<R, W>(
    reader: &mut R,
    writer: &mut W,
    idle_timeout: Duration,
) -> io::Result<u64>
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
{
    let mut buffer = vec![0u8; TUNNEL_BUFFER_SIZE];
    let mut copied = 0u64;
    loop {
        let read = timeout(idle_timeout, reader.read(&mut buffer))
            .await
            .map_err(|_| idle_timeout_error())??;
        if read == 0 {
            break;
        }
        timeout(idle_timeout, writer.write_all(&buffer[..read]))
            .await
            .map_err(|_| idle_timeout_error())??;
        copied += read as u64;
    }
    writer.shutdown().await?;
    Ok(copied)
}

fn idle_timeout_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::TimedOut,
        "tunnel idle timeout expired",
    )
}
