//! Optional authentication of clients that use this proxy.
//!
//! This is separate from SOCKS5 credentials: it protects the listener in
//! front of the proxy, not the connection behind it. The expected header
//! value is computed once at startup and compared byte for byte.

use base64::Engine;
use http::header::{HeaderValue, PROXY_AUTHENTICATE, PROXY_AUTHORIZATION};
use hyper::body::Incoming;
use hyper::{Request, Response};

use crate::proxy_http_forwarder::{full_body, BoxBody, StatusCode};

/// Optional inbound authentication for clients of this proxy.
///
/// The expected `Proxy-Authorization` value is precomputed once so that
/// checking a request is a single header lookup and byte comparison.
#[derive(Clone, Default)]
pub(crate) struct ProxyAuth {
    /// Complete `Basic <base64>` value, or `None` when auth is disabled.
    expected: Option<HeaderValue>,
}

impl ProxyAuth {
    /// Builds the credential check from the CLI options.
    ///
    /// The argument parser already guarantees that both halves are
    /// present together; a partial pair simply disables authentication.
    pub(crate) fn new(
        username: Option<String>,
        password: Option<String>,
    ) -> Self {
        match (username, password) {
            (Some(username), Some(password)) => {
                let raw = format!("{}:{}", username, password);
                let encoded = base64::engine::general_purpose::STANDARD
                    .encode(raw);
                let value = HeaderValue::from_str(&format!(
                    "Basic {}",
                    encoded
                ))
                    .expect("base64 credentials are valid header text");
                Self {
                    expected: Some(value),
                }
            }
            _ => Self { expected: None },
        }
    }

    /// Whether a request without credentials should be challenged.
    pub(crate) fn is_enabled(&self) -> bool {
        self.expected.is_some()
    }

    /// Returns `true` when authentication is disabled or the request
    /// carries the configured `Proxy-Authorization` value.
    pub(crate) fn is_authorized(
        &self,
        req: &Request<Incoming>,
    ) -> bool {
        let Some(expected) = self.expected.as_ref() else {
            return true;
        };
        req.headers()
            .get(PROXY_AUTHORIZATION)
            .map(|value| value == expected)
            .unwrap_or(false)
    }
}

/// Builds the `407` response that asks the client for credentials.
pub(crate) fn challenge() -> Response<BoxBody> {
    Response::builder()
        .status(StatusCode::PROXY_AUTHENTICATION_REQUIRED)
        .header(
            PROXY_AUTHENTICATE,
            "Basic realm=\"karako-http-socks5h-proxy\"",
        )
        .body(full_body("proxy authentication required"))
        .expect("valid proxy authentication response")
}
