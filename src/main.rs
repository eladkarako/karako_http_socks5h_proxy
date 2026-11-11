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

#[macro_use]
mod logging;

mod args;

mod body;

mod http;

mod proxy;

mod socket_config;

mod socks5;

mod tunnel;

mod http1_service;

mod http2_service;

mod upstream;

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

/// How long in-flight connections may take to finish after a shutdown
/// signal before they are aborted.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(8);

#[cfg(target_os = "windows")]
const EOL: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
const EOL: &str = "\n";

#[tokio::main]
async fn main() -> Result<()> {
    let args = args::parse_and_validate()?;
    logging::init(args.quiet, args.verbose);
    let auth = socks5::Socks5Auth::new(
        args.socks5_username.clone(),
        args.socks5_password.clone(),
    );

    let addr = std::net::SocketAddr::new(
        args.bind,
        args.http_port,
    );
    let upstream_addr = std::net::SocketAddr::new(
        args.upstream_host,
        args.upstream_port,
    );

    let listener =
        TcpListener::bind(addr).await.with_context(|| {
            format!("Failed to bind HTTP listener on {}", addr)
        })?;
    configure_listener_socket(&listener);

    log_info!(
        "HTTP/1.1 + HTTP/2 proxy listening on {}",
        addr
    );
    log_info!(
        "Forwarding to upstream SOCKS5h on {}",
        upstream_addr
    );
    log_debug!(
        "[LOG] Level is {} (use -v for handshake details, -vv for packet dumps)",
        if args.quiet {
            "error"
        } else {
            match args.verbose {
                0 => "info",
                1 => "debug",
                _ => "trace",
            }
        }
    );

    log_debug!(
        "[SOCKS5] Username/password authentication {}",
        if args.socks5_username.is_some() {
            "enabled"
        } else {
            "disabled"
        }
    );

    let shutdown = Arc::new(tokio::sync::Notify::new());
    let shutdown_clone = shutdown.clone();
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        log_info!(
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
            log_info!(
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
                log_info!(
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
    let upstream_client = upstream::build(upstream_addr, auth.clone());

    loop {
        tokio::select! {
            _ = shutdown.notified() => break,
            Some(result) = connections.join_next(), if !connections.is_empty() => {
                if let Err(error) = result {
                    log_error!("Connection task terminated unexpectedly: {}", error);
                }
            }
            result = listener.accept() => {
                match result {
                    Ok((socket, client_addr)) => {
                        let permit = match connection_limit
                            .clone()
                            .try_acquire_owned()
                        {
                            Ok(permit) => permit,
                            Err(_) => {
                                log_warn!(
                                    "[LIMIT] Refusing {}: {} connections already active",
                                    client_addr,
                                    MAX_CONCURRENT_CONNECTIONS
                                );
                                drop(socket);
                                continue;
                            }
                        };
                        let active = MAX_CONCURRENT_CONNECTIONS
                            - connection_limit.available_permits();
                        log_debug!(
                            "[ACCEPT] {} connected (active: {})",
                            client_addr,
                            active
                        );
                        configure_tokio_socket(
                            &socket,
                            SocketConfiguration::CLIENT_INBOUND,
                        );
                        let client = upstream_client.clone();
                        let auth = auth.clone();
                        connections.spawn(async move {
                            // Held for the lifetime of the connection so
                            // the semaphore reflects active handlers.
                            let _permit = permit;
                            if let Err(e) = proxy::handle_client_connection(socket, client_addr, upstream_addr, client, auth).await {
                                log_error!("Client handler error from {}: {:#}", client_addr, e);
                            }
                        });
                    }
                    Err(e) => {
                        log_warn!("Accept error: {}", e);
                        // Avoid a hot loop if the listener keeps failing.
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                }
            }
        }
    }

    let active = connections.len();
    log_info!(
        "Stopped accepting connections; draining {} active connection(s) (up to {}s).",
        active,
        SHUTDOWN_GRACE_PERIOD.as_secs()
    );

    // Give in-flight requests and tunnels a bounded chance to finish
    // instead of cutting them mid-stream.
    let drained = tokio::time::timeout(SHUTDOWN_GRACE_PERIOD, async {
        let mut drained = 0usize;
        while let Some(result) = connections.join_next().await {
            if let Err(error) = result {
                log_error!(
                    "Connection task terminated unexpectedly during shutdown: {}",
                    error
                );
            }
            drained += 1;
        }
        drained
    })
    .await;

    match drained {
        Ok(count) => log_info!(
            "Drained {} connection(s) cleanly.",
            count
        ),
        Err(_) => {
            let remaining = connections.len();
            log_warn!(
                "Grace period elapsed with {} connection(s) still active; aborting them.",
                remaining
            );
            connections.shutdown().await;
        }
    }

    Ok(())
}
