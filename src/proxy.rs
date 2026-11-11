use anyhow::Result;
use hyper::body::Incoming;
use hyper::service::Service;
use hyper::{Request, Response};
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::time::Duration;
use tokio::net::TcpStream;

use crate::http::{full_body, BadRequestError, BoxBody, StatusCode};
use crate::http1_service::handle_http1_request;
use crate::http2_service::handle_h2_request;

const HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy)]
struct ProxyService {
    upstream_port: u16,
    client_addr: SocketAddr,
}

impl Service<Request<Incoming>> for ProxyService {
    type Response = Response<BoxBody>;
    type Error = Infallible;
    type Future = Pin<
        Box<
            dyn Future<Output = Result<Self::Response, Self::Error>>
                + Send
                + 'static,
        >,
    >;

    fn call(&self, req: Request<Incoming>) -> Self::Future {
        let upstream_port = self.upstream_port;
        let client_addr = self.client_addr;
        Box::pin(async move {
            let result = match req.version() {
                hyper::Version::HTTP_2 => {
                    handle_h2_request(req, upstream_port, client_addr)
                        .await
                }
                _ => {
                    handle_http1_request(
                        req,
                        upstream_port,
                        client_addr,
                    )
                        .await
                }
            };
            Ok(result.unwrap_or_else(|e| {
                eprintln!(
                    "Request handling error from {}: {:#}",
                    client_addr, e
                );
                error_response(&e)
            }))
        })
    }
}

fn error_response(error: &anyhow::Error) -> Response<BoxBody> {
    let (status, message) =
        match error.downcast_ref::<BadRequestError>() {
            Some(bad_request) => {
                (StatusCode::BAD_REQUEST, bad_request.0.clone())
            }
            None => (
                StatusCode::BAD_GATEWAY,
                "upstream request failed".to_string(),
            ),
        };
    let body = full_body(message);
    Response::builder()
        .status(status)
        .body(body)
        .expect("valid error response")
}

pub async fn handle_client_connection(
    socket: TcpStream,
    addr: SocketAddr,
    upstream_port: u16,
) -> Result<()> {
    let io = hyper_util::rt::TokioIo::new(socket);
    let service = ProxyService {
        upstream_port,
        client_addr: addr,
    };
    let mut builder =
        hyper_util::server::conn::auto::Builder::new(
            hyper_util::rt::TokioExecutor::new(),
        );
    builder
        .http1()
        .timer(hyper_util::rt::TokioTimer::new())
        .preserve_header_case(true)
        .title_case_headers(false)
        .header_read_timeout(HEADER_READ_TIMEOUT);
    builder
        .http2()
        .timer(hyper_util::rt::TokioTimer::new())
        .enable_connect_protocol();
    match builder
        .serve_connection_with_upgrades(io, service)
        .await
    {
        Ok(()) => Ok(()),
        Err(e) => {
            eprintln!("Connection {} error: {}", addr, e);
            Ok(())
        }
    }
}
