use anyhow::Context;
use bytes::Bytes;
use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;

use crate::http::{
    self, build_forward_request, extract_host, extract_port,
    read_request_body, BoxBody,
};
use crate::socket_config::{
    configure_tokio_socket, SocketConfiguration,
};
use crate::socks5::{
    connect_to_socks5_server, establish_socks5_connection,
};
use crate::tunnel::handle_tunnel;

pub(crate) async fn handle_http1_request(
    req: Request<Incoming>,
    upstream_port: u16,
    client_addr: std::net::SocketAddr,
) -> anyhow::Result<Response<BoxBody>> {
    let method = req.method().clone();
    let uri = req.uri().clone();
    let host = extract_host(&uri, &req)?;
    let port = extract_port(&uri);
    eprintln!(
        "[HTTP/1.1] {} {} (host: {}:{}) from {}",
        method, uri, host, port, client_addr
    );
    match method.as_str() {
        "CONNECT" => {
            eprintln!(
                "[CONNECT TUNNEL] Initiating tunnel to {}:{}",
                host, port
            );
            let target_stream =
                connect_to_socks5_server(upstream_port).await?;
            let mut target_stream_clone = target_stream;
            establish_socks5_connection(
                &mut target_stream_clone,
                &host,
                port,
            )
                .await?;
            configure_tokio_socket(
                &target_stream_clone,
                SocketConfiguration::SOCKS_TUNNEL,
            )?;
            eprintln!(
                "[CONNECT TUNNEL] SOCKS5 connection established, sending 200 OK"
            );
            let response = Response::builder()
                .status(StatusCode::OK)
                .header("Connection", "Upgrade")
                .body(Full::new(Bytes::new()).boxed())
                .context("Failed to build CONNECT response")?;
            tokio::spawn(async move {
                match hyper::upgrade::on(req).await {
                    Ok(upgraded) => {
                        eprintln!(
                            "[CONNECT TUNNEL] Upgrade successful for {}:{}",
                            host, port
                        );
                        if let Err(e) = handle_tunnel(
                            upgraded,
                            target_stream_clone,
                            &host,
                            port,
                        )
                            .await
                        {
                            eprintln!(
                                "[CONNECT TUNNEL] Tunnel error for {}:{}: {:#}",
                                host, port, e
                            );
                        }
                    }
                    Err(e) => {
                        eprintln!(
                            "[CONNECT TUNNEL] Upgrade failed for {}:{}: {}",
                            host, port, e
                        );
                    }
                }
            });
            Ok(response)
        }
        _ => {
            let mut req_clone = req;
            let body_bytes =
                read_request_body(&mut req_clone).await?;
            let forward_req = build_forward_request(
                &method,
                &uri,
                &host,
                port,
                &req_clone,
                &body_bytes,
            )?;
            let upstream_stream =
                connect_to_socks5_server(upstream_port).await?;
            let mut upstream_stream_clone = upstream_stream;
            establish_socks5_connection(
                &mut upstream_stream_clone,
                &host,
                port,
            )
                .await?;
            let response = http::send_http_request(
                upstream_stream_clone,
                forward_req,
                &method,
            )
                .await?;
            let (parts, body) = response.into_parts();
            let body_bytes = if matches!(method.as_str(), "HEAD")
                || matches!(
                    parts.status,
                    StatusCode::NO_CONTENT
                        | StatusCode::NOT_MODIFIED
                ) {
                Bytes::new()
            } else {
                BodyExt::collect(body).await?.to_bytes()
            };
            Ok(Response::from_parts(
                parts,
                Full::new(body_bytes).boxed(),
            ))
        }
    }
}
