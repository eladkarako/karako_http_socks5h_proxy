use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use std::convert::Infallible;

pub(crate) use http::{Request, Response, StatusCode};

/// Type-erased body used for every proxy response.
///
/// The error is boxed so that a streamed upstream body can be passed
/// through without collecting it in memory.
pub(crate) type BoxBody = http_body_util::combinators::UnsyncBoxBody<
    Bytes,
    Box<dyn std::error::Error + Send + Sync>,
>;

pub(crate) fn empty_body() -> BoxBody {
    Empty::<Bytes>::new()
        .map_err(|never: Infallible| boxed_error(never))
        .boxed_unsync()
}

pub(crate) fn full_body(body: impl Into<Bytes>) -> BoxBody {
    Full::new(body.into())
        .map_err(|never: Infallible| boxed_error(never))
        .boxed_unsync()
}

pub(crate) fn box_incoming(body: Incoming) -> BoxBody {
    body.map_err(boxed_error).boxed_unsync()
}

fn boxed_error(
    error: impl std::error::Error + Send + Sync + 'static,
) -> Box<dyn std::error::Error + Send + Sync> {
    Box::new(error)
}

pub(crate) struct Target {
    pub host: String,
    pub port: u16,
}

/// Parses the request target, falling back to the `Host` header when the
/// request URI only carries an origin-form path.
///
/// IPv6 literal hosts are kept in bracketed form (for example `[::1]`) so
/// that they stay valid inside URLs and `Host` headers.
pub(crate) fn extract_target(
    req: &Request<Incoming>,
) -> Result<Target> {
    let uri = req.uri();
    if let Some(authority) = uri.authority() {
        return Ok(Target {
            host: authority.host().to_string(),
            port: authority
                .port_u16()
                .unwrap_or_else(|| default_port(uri, req)),
        });
    }
    let host_header = req
        .headers()
        .get(http::header::HOST)
        .context("Request has neither an authority nor a Host header")?;
    let host_str = host_header
        .to_str()
        .context("Invalid Host header encoding")?;
    let authority: http::uri::Authority = host_str
        .parse()
        .context("Invalid Host header authority")?;
    Ok(Target {
        host: authority.host().to_string(),
        port: authority
            .port_u16()
            .unwrap_or_else(|| default_port(uri, req)),
    })
}

fn default_port(
    uri: &hyper::Uri,
    req: &Request<Incoming>,
) -> u16 {
    match uri.scheme_str() {
        Some("https") => 443,
        Some("http") => 80,
        _ if req.method() == http::Method::CONNECT => 443,
        _ => 80,
    }
}

pub fn build_forward_request(
    method: &http::Method,
    uri: &hyper::Uri,
    host: &str,
    port: u16,
    req: Request<Incoming>,
) -> Result<Request<Incoming>> {
    let path_query = uri
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| "/".to_string());
    let forward_url =
        format!("http://{}:{}{}", host, port, path_query);
    let (parts, body) = req.into_parts();
    let connection_tokens =
        connection_header_tokens(&parts.headers);
    let mut forward_req = Request::builder()
        .method(method.clone())
        .uri(forward_url)
        .version(hyper::Version::HTTP_11);
    for (name, value) in parts.headers.iter() {
        let name = name.as_str();
        if name == "host" || name == "content-length" {
            continue;
        }
        if is_hop_by_hop(name, &connection_tokens) {
            continue;
        }
        forward_req = forward_req.header(name, value);
    }
    forward_req = forward_req.header(
        http::header::HOST,
        format!("{}:{}", host, port),
    );
    forward_req
        .body(body)
        .context("Failed to build forward request")
}

/// Headers that only apply to a single transport hop and must not be
/// forwarded by a proxy (RFC 9110 section 7.6.1).
const HOP_BY_HOP_HEADERS: &[&str] = &[
    "connection",
    "keep-alive",
    "proxy-connection",
    "proxy-authenticate",
    "proxy-authorization",
    "te",
    "trailer",
    "transfer-encoding",
    "upgrade",
];

/// Header names listed in the `Connection` header are hop-by-hop as
/// well, even if they are not in the standard list above.
fn connection_header_tokens(
    headers: &http::HeaderMap,
) -> Vec<String> {
    headers
        .get_all(http::header::CONNECTION)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(','))
        .map(|token| token.trim().to_ascii_lowercase())
        .filter(|token| !token.is_empty())
        .collect()
}

fn is_hop_by_hop(name: &str, connection_tokens: &[String]) -> bool {
    HOP_BY_HOP_HEADERS.contains(&name)
        || connection_tokens.iter().any(|token| token == name)
}

fn should_forward_response_header(
    name: &str,
    method: &http::Method,
    connection_tokens: &[String],
) -> bool {
    if is_hop_by_hop(name, connection_tokens) {
        return false;
    }
    !(name == "content-length" && method != http::Method::HEAD)
}

pub async fn send_http_request(
    upstream_stream: tokio::net::TcpStream,
    forward_req: Request<Incoming>,
    method: &http::Method,
) -> Result<Response<BoxBody>> {
    let io = hyper_util::rt::TokioIo::new(upstream_stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new()
        .handshake(io)
        .await
        .context("Failed to establish HTTP/1.1 connection through SOCKS5")?;
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            eprintln!("Upstream HTTP/1.1 connection error: {}", e);
        }
    });
    let response = sender
        .send_request(forward_req)
        .await
        .context("Failed to send HTTP/1.1 request through SOCKS5")?;
    let (parts, body) = response.into_parts();
    let connection_tokens =
        connection_header_tokens(&parts.headers);
    let mut response_builder =
        Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if should_forward_response_header(
            name.as_str(),
            method,
            &connection_tokens,
        ) {
            response_builder = response_builder.header(name, value);
        }
    }
    Ok(response_builder
        .body(box_incoming(body))
        .context("Failed to build response")?)
}
