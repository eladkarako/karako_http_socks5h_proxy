use anyhow::{Context, Result};
use std::io;
use std::time::Duration;
use tokio::io::{
    AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt,
};
use tokio::net::TcpStream;
use tokio::time::timeout;

/// How long a tunnel may sit without any traffic in either direction
/// before it is torn down.
const TUNNEL_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

const TUNNEL_BUFFER_SIZE: usize = 16 * 1024;

pub async fn handle_tunnel(
    upgraded: hyper::upgrade::Upgraded,
    target: TcpStream,
    host: &str,
    port: u16,
) -> Result<()> {
    eprintln!(
        "[TUNNEL] Starting bidirectional tunnel for {}:{}",
        host, port
    );
    let client = hyper_util::rt::TokioIo::new(upgraded);
    let (mut client_read, mut client_write) = tokio::io::split(client);
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
    eprintln!(
        "[TUNNEL] Tunnel {}:{} closed normally: client→target: {} bytes, target→client: {} bytes",
        host, port, client_to_target, target_to_client
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
