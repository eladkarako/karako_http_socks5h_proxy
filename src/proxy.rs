use anyhow::{bail, Result};
use std::net::SocketAddr;
use tokio::net::TcpStream;

use crate::http1_service::handle_http1_client_connection;
use crate::http2_service::handle_http2_client_connection;

pub async fn handle_client_connection(
    socket: TcpStream,
    addr: SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    let mut peek_buf = [0u8; 24];
    match socket.peek(&mut peek_buf).await {
        Ok(n) if n >= 4 => {
            let peek_str =
                String::from_utf8_lossy(&peek_buf[..n.min(24)]);
            if peek_str.starts_with("PRI * HTTP/2.0") {
                eprintln!(
                    "[DETECT] HTTP/2 preface detected for {}",
                    addr
                );
                handle_http2_client_connection(
                    socket,
                    addr,
                    upstream_port,
                )
                .await
            } else if peek_str.starts_with("HTTP/")
                || peek_str.contains("CONNECT ")
            {
                eprintln!("[DETECT] HTTP/1.1 detected for {}", addr);
                handle_http1_client_connection(
                    socket,
                    addr,
                    upstream_port,
                )
                .await
            } else {
                bail!(
                    "Unknown protocol from {}: {:?}",
                    addr,
                    &peek_buf[..n.min(16)]
                )
            }
        }
        Ok(n) if n > 0 => {
            bail!(
                "Failed to peek protocol from {} (got {} bytes, need >= 4)",
                addr,
                n
            )
        }
        Ok(_) => {
            eprintln!(
                "[DETECT] Empty connection from {} (closed immediately)",
                addr
            );
            Ok(())
        }
        Err(e) => {
            bail!("Failed to peek protocol from {}: {}", addr, e)
        }
    }
}
