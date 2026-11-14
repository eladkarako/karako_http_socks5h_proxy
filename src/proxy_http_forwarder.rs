use anyhow::{Context, Result};
use bytes::Bytes;
use http_body_util::{BodyExt, Empty, Full};
use hyper::body::Incoming;
use std::convert::Infallible;

use crate::proxy_error_types::ProxyError;

pub(crate) use http::{Request, Response, StatusCode};

/// How long an upstream response body may go without producing a frame
/// before the stream is aborted.
const UPSTREAM_BODY_IDLE_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(60);

/// How long to wait for upstream response headers before giving up.
const UPSTREAM_RESPONSE_HEADER_TIMEOUT: std::time::Duration =
    std::time::Duration::from_secs(120);

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
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}

pub(crate) fn full_body(body: impl Into<Bytes>) -> BoxBody {
    Full::new(body.into())
        .map_err(|never: Infallible| match never {})
        .boxed_unsync()
}

pub(crate) fn box_incoming(body: Incoming) -> BoxBody {
    crate::body_idle_timeout::IdleTimeoutBody::new(
        body,
        UPSTREAM_BODY_IDLE_TIMEOUT,
    )
        .boxed_unsync()
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
    req: &Request<Incoming>
) -> Result<Target> {
    let uri = req.uri();
    if let Some(scheme) = uri.scheme_str() {
        match scheme {
            "http" => {}
            "https" => {
                return Err(ProxyError::BadRequest(
                    "HTTPS request targets must use CONNECT; \
                     forward-proxy TLS is not supported"
                        .to_string(),
                )
                    .into());
            }
            other => {
                return Err(ProxyError::BadRequest(format!(
                    "unsupported request target scheme: {}",
                    other
                ))
                    .into());
            }
        }
    }
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
        .ok_or_else(|| {
            ProxyError::BadRequest(
                "request has neither an authority nor a Host header"
                    .to_string(),
            )
        })?;
    let host_str = host_header.to_str().map_err(|_| {
        ProxyError::BadRequest("Host header is not valid ASCII".to_string())
    })?;
    let authority: http::uri::Authority =
        host_str.parse().map_err(|_| {
            ProxyError::BadRequest(format!(
                "invalid Host header authority: {}",
                host_str
            ))
        })?;
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

/// Request target that is safe to print at default verbosity: the path
/// without the query string, or the authority for CONNECT-style targets.
pub(crate) fn loggable_target(uri: &hyper::Uri) -> &str {
    if uri.path().is_empty() {
        uri.authority()
            .map(|authority| authority.as_str())
            .unwrap_or("/")
    } else {
        uri.path()
    }
}

/// Controls optional headers added by the proxy.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ForwardingOptions {
    /// Add the RFC 9110 `Via` header to forwarded messages.
    pub add_via: bool,
    /// Add `X-Forwarded-For` with the immediate client address.
    pub add_x_forwarded_for: bool,
}

/// Value used for the RFC 9110 `Via` header.
///
/// The version reflects the protocol of the message that entered the
/// proxy, and the pseudonym identifies this proxy.
pub(crate) fn via_value(version: http::Version) -> String {
    let protocol = match version {
        http::Version::HTTP_10 => "1.0",
        http::Version::HTTP_2 => "2",
        http::Version::HTTP_3 => "3",
        _ => "1.1",
    };
    format!("{} {}", protocol, env!("CARGO_PKG_NAME"))
}

pub fn build_forward_request(
    method: &http::Method,
    uri: &hyper::Uri,
    host: &str,
    port: u16,
    req: Request<Incoming>,
    client_addr: std::net::SocketAddr,
    forwarding: ForwardingOptions,
) -> Result<Request<Incoming>> {
    let path_query = uri
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| "/".to_string());
    let forward_url =
        format!("http://{}:{}{}", host, port, path_query);
    let client_version = req.version();
    let (parts, body) = req.into_parts();
    let connection_tokens = connection_header_tokens(&parts.headers);
    let mut forward_req = Request::builder()
        .method(method.clone())
        .uri(forward_url)
        .version(hyper::Version::HTTP_11);
    for (name, value) in parts.headers.iter() {
        let name = name.as_str();
        // `Host` is rebuilt for the upstream authority below. Every other
        // end-to-end header, including `Content-Length`, is passed through
        // unchanged because the body is forwarded verbatim.
        if name == "host" {
            continue;
        }
        // When we add X-Forwarded-For ourselves, drop the client-supplied
        // value so it cannot be used to spoof the original address.
        if forwarding.add_x_forwarded_for
            && name.eq_ignore_ascii_case("x-forwarded-for")
        {
            continue;
        }
        if is_hop_by_hop(name, &connection_tokens) {
            log_trace!(
                "[HTTP] Dropping hop-by-hop header: {}",
                name
            );
            continue;
        }
        forward_req = forward_req.header(name, value);
    }
    forward_req = forward_req.header(
        http::header::HOST,
        host_header_value(host, port),
    );
    if forwarding.add_via {
        forward_req = forward_req
            .header(http::header::VIA, via_value(client_version));
    }
    if forwarding.add_x_forwarded_for {
        forward_req = forward_req.header(
            "x-forwarded-for",
            client_addr.ip().to_string(),
        );
    }
    forward_req
        .body(body)
        .context("Failed to build forward request")
}

fn host_header_value(
    host: &str,
    port: u16,
) -> String {
    // `host` already carries brackets for IPv6 literals. Default ports are
    // omitted because some origin servers match on the bare host name.
    if port == 80 {
        host.to_string()
    } else {
        format!("{}:{}", host, port)
    }
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
    headers: &http::HeaderMap
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

fn is_hop_by_hop(
    name: &str,
    connection_tokens: &[String],
) -> bool {
    HOP_BY_HOP_HEADERS.contains(&name)
        || connection_tokens
        .iter()
        .any(|token| token == name)
}

fn should_forward_response_header(
    name: &str,
    connection_tokens: &[String],
) -> bool {
    // `Content-Length` is end-to-end and stays valid because the body is
    // streamed through untouched. Dropping it would force chunked encoding.
    !is_hop_by_hop(name, connection_tokens)
}

pub async fn send_http_request(
    client: &crate::upstream_http_client::UpstreamClient,
    forward_req: Request<Incoming>,
    client_version: http::Version,
    forwarding: ForwardingOptions,
) -> Result<Response<BoxBody>> {
    let started = std::time::Instant::now();
    log_debug!(
        "[UPSTREAM] {} {} ({} header(s))",
        forward_req.method(),
        loggable_target(forward_req.uri()),
        forward_req.headers().len()
    );
    log_trace!("[UPSTREAM] Full target: {}", forward_req.uri());
    if tracing::enabled!(tracing::Level::TRACE) {
        for (name, value) in forward_req.headers().iter() {
            log_trace!(
                "[UPSTREAM] Request header: {}: {}",
                name,
                value.to_str().unwrap_or("<binary>")
            );
        }
    }
    let response = match tokio::time::timeout(
        UPSTREAM_RESPONSE_HEADER_TIMEOUT,
        client.request(forward_req),
    )
        .await
    {
        Ok(result) => result
            .context("Failed to send HTTP/1.1 request through SOCKS5")?,
        Err(_) => {
            return Err(ProxyError::Timeout {
                phase: "waiting for upstream response headers",
                duration: UPSTREAM_RESPONSE_HEADER_TIMEOUT,
            }
                .into());
        }
    };
    let (parts, body) = response.into_parts();
    log_debug!(
        "[UPSTREAM] -> {} in {:?} ({} response header(s))",
        parts.status,
        started.elapsed(),
        parts.headers.len()
    );
    let connection_tokens = connection_header_tokens(&parts.headers);
    let mut response_builder =
        Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        if should_forward_response_header(
            name.as_str(),
            &connection_tokens,
        ) {
            response_builder = response_builder.header(name, value);
        }
    }
    if forwarding.add_via {
        response_builder = response_builder
            .header(http::header::VIA, via_value(client_version));
    }
    response_builder
        .body(box_incoming(body))
        .context("Failed to build response")
}
