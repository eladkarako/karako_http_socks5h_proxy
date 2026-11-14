//! Entry point and connection lifecycle.
//!
//! Startup goes: parse arguments, install logging, publish buffer tuning,
//! build the shared upstream client, bind the listener, then accept until
//! a shutdown signal arrives. Every accepted socket gets a permit from a
//! semaphore so the process cannot be pushed into unbounded concurrency.
//!
//! Shutdown stops accepting, gives in-flight connections a short grace
//! period, and aborts whatever is left. A second Ctrl+C skips the grace
//! period entirely.

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
mod logging_output_setup;

mod command_line_args;

mod body_idle_timeout;

mod proxy_http_forwarder;

mod http_proxy_service;

mod inbound_proxy_auth;

mod proxy_error_types;

mod response_header_policy;

mod tcp_socket_config;

mod socks5_handshake_flow;

mod http_connect_tunnel;

mod runtime_buffer_tuning;

mod http1_request_handler;

mod http2_request_handler;

mod upstream_http_client;

use anyhow::{Context, Result};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;
use tokio::task::JoinSet;

use crate::tcp_socket_config::{
    configure_listener_socket, configure_tokio_socket,
    SocketConfiguration,
};

/// Upper bound on simultaneously handled client connections.
const MAX_CONCURRENT_CONNECTIONS: usize = 1024;

/// How long in-flight connections may take to finish after a shutdown
/// signal before they are aborted.
const SHUTDOWN_GRACE_PERIOD: Duration = Duration::from_secs(8);

#[cfg(target_os = "windows")]
/// Line ending used in multi-line console messages.
const EOL: &str = "\r\n";

#[cfg(not(target_os = "windows"))]
/// Line ending used in multi-line console messages.
const EOL: &str = "\n";

#[tokio::main]
async fn main() -> Result<()> {
    let args = command_line_args::parse_and_validate()?;
    let _log_guard = logging_output_setup::init(args.quiet, args.verbose);
    runtime_buffer_tuning::init(runtime_buffer_tuning::Tuning {
        socket_recv_buffer: args.socket_recv_buffer_bytes(),
        socket_send_buffer: args.socket_send_buffer_bytes(),
        tunnel_buffer_size: args.tunnel_buffer_bytes(),
    });
    log_debug!(
        "[TUNING] tunnel copy buffer: {} KiB, socket recv: {}, socket send: {}",
        args.tunnel_buffer_size,
        args.socket_recv_buffer
            .map(|v| format!("{} KiB", v))
            .unwrap_or_else(|| "per-role".to_string()),
        args.socket_send_buffer
            .map(|v| format!("{} KiB", v))
            .unwrap_or_else(|| "per-role".to_string())
    );
    let auth = socks5_handshake_flow::Socks5Auth::new(
        args.socks5_username.clone(),
        args.socks5_password.clone(),
    );
    let proxy_auth = inbound_proxy_auth::ProxyAuth::new(
        args.proxy_username.clone(),
        args.proxy_password.clone(),
    );
    let forwarding = proxy_http_forwarder::ForwardingOptions {
        add_via: !args.no_via,
        add_x_forwarded_for: args.x_forwarded_for,
    };
    let response_policy = response_header_policy::ResponsePolicy {
        boostaler_cache: args.boostaler_cache,
        cors_allow_all: args.cors_allow_all,
        strip_security_headers: args.strip_security_headers,
    };

    let addr = std::net::SocketAddr::new(args.bind, args.http_port);
    let upstream_addr = std::net::SocketAddr::new(
        args.upstream_host,
        args.upstream_port,
    );

    let listener = TcpListener::bind(addr)
        .await
        .with_context(|| {
            format!("Failed to bind HTTP listener on {}", addr)
        })?;
    configure_listener_socket(&listener);

    log_info!("HTTP/1.1 + HTTP/2 proxy listening on {}", addr);
    log_info!("Forwarding to upstream SOCKS5h on {}", upstream_addr);
    if !args.bind.is_loopback() {
        if proxy_auth.is_enabled() {
            log_warn!(
                "Listening on non-loopback address {} with inbound \
                 authentication enabled.",
                args.bind
            );
        } else {
            log_warn!(
                "Listening on non-loopback address {} WITHOUT inbound \
                 authentication: anyone who can reach this port can use \
                 the upstream SOCKS5 proxy.",
                args.bind
            );
        }
    }
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
    // Ctrl+C is handled in its own task so the accept loop only has to
    // watch one shutdown signal.
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            log_warn!("Failed to listen for Ctrl+C");
            return;
        }
        log_info!(
            "{}Ctrl+C received, draining connections... \
             (press Ctrl+C again to exit immediately)",
            EOL
        );
        shutdown_clone.notify_one();
        if tokio::signal::ctrl_c().await.is_ok() {
            log_warn!(
                "{}Second Ctrl+C received, exiting immediately.",
                EOL
            );
            std::process::exit(130);
        }
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

        let shutdown_clone = shutdown.clone();
        tokio::spawn(async move {
            if let Ok(mut ctrl_close) =
                tokio::signal::windows::ctrl_close()
            {
                ctrl_close.recv().await;
                log_info!(
                    "{}Console close received, shutting down gracefully...",
                    EOL
                );
                shutdown_clone.notify_one();
            }
        });
    }

    let connection_limit =
        Arc::new(Semaphore::new(MAX_CONCURRENT_CONNECTIONS));
    let mut connections = JoinSet::new();
    let upstream_client =
        upstream_http_client::build(upstream_addr, auth.clone());
    let service_config = http_proxy_service::ProxyServiceConfig {
        upstream_addr,
        client: upstream_client,
        auth,
        inbound_auth: proxy_auth,
        forwarding,
        response_policy,
    };

    loop {
        // `join_next` doubles as a way to reap finished tasks while
        // waiting, so a failing task is reported promptly instead of
        // accumulating in the set.
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
                        let config = service_config.clone();
                        connections.spawn(async move {
                            // Held for the lifetime of the connection so
                            // the semaphore reflects active handlers.
                            let _permit = permit;
                            if let Err(e) = http_proxy_service::handle_client_connection(socket, client_addr, config).await {
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
        "Stopped accepting connections; draining {} active connection(s) for up to {}s.",
        active,
        SHUTDOWN_GRACE_PERIOD.as_secs()
    );
    if active > 0 {
        log_warn!(
            "CONNECT tunnels have a {}s idle timeout, so long-lived tunnels \
             still active after {}s will be aborted.",
            crate::http_connect_tunnel::TUNNEL_IDLE_TIMEOUT.as_secs(),
            SHUTDOWN_GRACE_PERIOD.as_secs()
        );
    }

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
        Ok(count) => {
            log_info!("Drained {} connection(s) cleanly.", count)
        }
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
