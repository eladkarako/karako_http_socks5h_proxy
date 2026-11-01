use anyhow::{bail, Context, Result};
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::TcpStream;
use tokio::time::timeout;

const TUNNEL_TIMEOUT: Duration = Duration::from_secs(600);

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
    let mut client = hyper_util::rt::TokioIo::new(upgraded);
    let mut target = target;
    let result = timeout(
        TUNNEL_TIMEOUT,
        copy_bidirectional(&mut client, &mut target),
    )
        .await;
    match result {
        Ok(Ok((client_to_target, target_to_client))) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} closed normally: client→target: {} bytes, target→client: {} bytes",
                host, port, client_to_target, target_to_client
            );
            Ok(())
        }
        Ok(Err(e)) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} copy error: {}",
                host, port, e
            );
            Err(e).context("Bidirectional copy failed")
        }
        Err(_) => {
            eprintln!(
                "[TUNNEL] Tunnel {}:{} timeout after {:?}",
                host, port, TUNNEL_TIMEOUT
            );
            bail!("Tunnel timeout")
        }
    }
}
