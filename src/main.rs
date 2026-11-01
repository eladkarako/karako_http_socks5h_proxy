#[cfg(all(
    target_os = "linux",
    any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64"
    )
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod http;

mod proxy;

mod socket_config;

mod socks5;

use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use anyhow::{ensure, Context, Result};
use clap::Parser;
use std::sync::Arc;
use tokio::net::TcpListener;

#[cfg(target_os = "windows")]
const EOL: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
const EOL: &str = "\n";

#[derive(Parser, Debug, Clone)]
#[command(name = env!("CARGO_PKG_NAME"))]
#[command(version = env!("CARGO_PKG_VERSION"))]
#[command(author = env!("CARGO_PKG_AUTHORS"))]
#[command(about=env!("CARGO_PKG_DESCRIPTION"),long_about=None)]
#[command(after_help = concat!("Repository:    ", env!("CARGO_PKG_REPOSITORY")))]
struct Args {
    #[arg(short = 'p', long, default_value = "8765")]
    http_port: u16,
    #[arg(short = 'u', long, default_value = "5678")]
    upstream_port: u16,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    ensure!(
        args.http_port != args.upstream_port,
        "HTTP port ({}) cannot be the same as upstream port ({})",
        args.http_port,
        args.upstream_port
    );
    let addr: std::net::SocketAddr =
        format!("127.0.0.1:{}", args.http_port)
            .parse()
            .context("Failed to parse listen address")?;
    let listener =
        TcpListener::bind(&addr).await.with_context(|| {
            format!(
                "Failed to bind HTTP listener on 127.0.0.1:{}",
                args.http_port
            )
        })?;
    eprintln!(
        "HTTP/1.1 + HTTP/2 proxy listening on 127.0.0.1:{}",
        args.http_port
    );
    eprintln!(
        "Forwarding to upstream SOCKS5h on 127.0.0.1:{}",
        args.upstream_port
    );
    let shutdown = Arc::new(tokio::sync::Notify::new());
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        eprintln!(
            "{}Ctrl+C received, shutting down gracefully...",
            EOL
        );
        shutdown_clone.notify_waiters();
    });

    #[cfg(unix)]
    {
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            let mut sigterm = tokio::signal::unix::signal(
                tokio::signal::unix::SignalKind::terminate(),
            )
                .context("Failed to listen for SIGTERM")?;
            sigterm.recv().await;
            eprintln!(
                "{}SIGTERM received, shutting down gracefully...",
                EOL
            );
            shutdown_clone.notify_waiters();
            Ok::<(), anyhow::Error>(())
        });
    }

    #[cfg(windows)]
    {
        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut ctrl_break) =
                tokio::signal::windows::ctrl_break()
            {
                ctrl_break.recv().await;
                eprintln!(
                    "{}Ctrl+Break received, shutting down gracefully...",
                    EOL
                );
                shutdown_clone.notify_waiters();
            }
        });
    }
    loop {
        tokio::select! {_=shutdown.notified()=>break,result=listener.accept()=>{
                match result{
                    Ok((socket,client_addr))=>{
        if let Err(e)=configure_tokio_socket(&socket,SocketConfiguration::CLIENT_INBOUND){
            eprintln!("[SOCKET] Failed to configure inbound client socket: {:#}",e);
        }
                        let upstream_port=args.upstream_port;tokio::spawn(async move{if let Err(e)=proxy::handle_client_connection(socket,client_addr,upstream_port).await{eprintln!("Client handler error from {}: {:#}",client_addr,e);}});}Err(e)=>{eprintln!("Accept error: {}",e);}}}}
    }
    eprintln!("Stopped accepting connections.");
    Ok(())
}
