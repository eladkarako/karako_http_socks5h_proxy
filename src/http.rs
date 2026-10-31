use anyhow::{Context, Result};
use bytes::Bytes;
pub(crate) use http::{Request, Response, StatusCode};
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
const MAX_HOSTNAME_LEN: usize = 255;
pub fn extract_host(
    uri: &hyper::Uri,
    req: &Request<Incoming>,
) -> Result<String> {
    if let Some(host) = uri.host() {
        return Ok(host.to_string());
    }
    if let Some(host_header) = req.headers().get("Host") {
        let host_str =
            host_header.to_str().context("Invalid Host header")?;
        if let Some((host, _port)) = host_str.split_once(':') {
            return Ok(host.to_string());
        }
        return Ok(host_str.to_string());
    }
    anyhow::bail!("Cannot determine host from request")
}
pub fn extract_port(uri: &hyper::Uri) -> u16 {
    uri.port_u16().unwrap_or(80)
}
pub async fn read_request_body(
    req: &mut Request<Incoming>
) -> Result<Vec<u8>> {
    use http_body_util::BodyExt;
    let body = req.body_mut();
    let collected = body
        .collect()
        .await
        .context("Error reading request body")?;
    Ok(collected.to_bytes().to_vec())
}
pub fn build_forward_request(
    method: &http::Method,
    uri: &hyper::Uri,
    host: &str,
    port: u16,
    req: &Request<Incoming>,
    body_bytes: &[u8],
) -> Result<Request<Full<Bytes>>> {
    let path_query = uri
        .path_and_query()
        .map(|pq| pq.to_string())
        .unwrap_or_else(|| "/".to_string());
    let forward_url =
        format!("http://{}:{}{}", host, port, path_query);
    let mut forward_req = Request::builder()
        .method(method.clone())
        .uri(forward_url)
        .version(hyper::Version::HTTP_11);
    for (name, value) in req.headers().iter() {
        let name_str = name.as_str();
        if should_forward_header(&name_str) {
            forward_req = forward_req.header(name, value);
        }
    }
    forward_req =
        forward_req.header("Host", format!("{}:{}", host, port));
    forward_req = forward_req
        .header("Content-Length", body_bytes.len().to_string());
    forward_req
        .body(Full::new(Bytes::from(body_bytes.to_vec())))
        .context("Failed to build forward request")
}
pub fn should_forward_response_header(
    name: &str,
    method: &http::Method,
) -> bool {
    let name_lower = name.to_lowercase();
    if matches!(
        name_lower.as_str(),
        "host"
            | "connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
            | "keep-alive"
    ) {
        return false;
    }
    if name_lower == "content-length" && method.as_str() != "HEAD" {
        return false;
    }
    true
}
pub fn should_forward_header(name: &str) -> bool {
    let name_lower = name.to_lowercase();
    !matches!(
        name_lower.as_str(),
        "host"
            | "connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
            | "proxy-connection"
            | "keep-alive"
            | "content-length"
    )
}
pub async fn send_http_request(
    upstream_stream: tokio::net::TcpStream,
    forward_req: Request<Full<Bytes>>,
    method: &http::Method,
) -> Result<Response<Full<Bytes>>> {
    let io = hyper_util::rt::TokioIo::new(upstream_stream);
    let (mut sender, conn) = hyper::client::conn::http1::Builder::new().handshake(io).await.context("Failed to establish HTTP/1.1 connection through SOCKS5")?;
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
    let body_bytes = if matches!(method.as_str(), "HEAD")
        || matches!(
            parts.status,
            StatusCode::NO_CONTENT | StatusCode::NOT_MODIFIED
        ) {
        Bytes::new()
    } else {
        BodyExt::collect(body)
            .await
            .context("Failed to read response body")?
            .to_bytes()
    };
    let mut response_builder =
        Response::builder().status(parts.status);
    for (name, value) in parts.headers.iter() {
        let name_str = name.as_str();
        if should_forward_response_header(&name_str, method) {
            response_builder = response_builder.header(name, value);
        }
    }
    if method.as_str() != "HEAD" {
        response_builder = response_builder
            .header("Content-Length", body_bytes.len().to_string());
    }
    Ok(response_builder
        .body(Full::new(body_bytes))
        .context("Failed to build response")?)
}
