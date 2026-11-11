#[cfg(all(
    any(target_os = "linux", target_os = "windows"),
    any(
        target_arch = "x86_64",
        target_arch = "x86",
        target_arch = "aarch64"
    )
))]
#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

mod args;

mod http;

mod proxy;

mod socket_config;

mod socks5;

mod tunnel;

mod http1_service;

mod http2_service;

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::socket_config::{
    configure_listener_socket, configure_tokio_socket,
    SocketConfiguration,
};

/// Upper bound on simultaneously handled client connections.
const MAX_CONCURRENT_CONNECTIONS: usize = 1024;

#[cfg(target_os = "windows")]
const EOL: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
const EOL: &str = "\n";

#[tokio::main]
async fn main() -> Result<()> {
    let args = args::parse_and_validate()?;

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
    configure_listener_socket(&listener);

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
        shutdown_clone.notify_one();
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
            shutdown_clone.notify_one();
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
                shutdown_clone.notify_one();
            }
        });
    }

    let connection_limit = Arc::new(Semaphore::new(
        MAX_CONCURRENT_CONNECTIONS,
    ));
    let mut connections = JoinSet::new();

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            Some(_) = connections.join_next(), if !connections.is_empty() => {}
            result = listener.accept() => {
                match result {
                    Ok((socket, client_addr)) => {
                        let permit = match connection_limit
                            .clone()
                            .try_acquire_owned()
                        {
                            Ok(permit) => permit,
                            Err(_) => {
                                eprintln!(
                                    "[LIMIT] Refusing {}: {} connections already active",
                                    client_addr,
                                    MAX_CONCURRENT_CONNECTIONS
                                );
                                drop(socket);
                                continue;
                            }
                        };
                        configure_tokio_socket(
                            &socket,
                            SocketConfiguration::CLIENT_INBOUND,
                        );
                        let upstream_port = args.upstream_port;
                        connections.spawn(async move {
                            // Held for the lifetime of the connection so
                            // the semaphore reflects active handlers.
                            let _permit = permit;
                            if let Err(e) = proxy::handle_client_connection(socket, client_addr, upstream_port).await {
                                eprintln!("Client handler error from {}: {:#}", client_addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        eprintln!("Accept error: {}", e);
                        // Avoid a hot loop if the listener keeps failing.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    eprintln!(
        "Stopped accepting connections; closing {} active connection(s).",
        connections.len()
    );
    connections.shutdown().await;
    Ok(())
}
