//! Optional rewriting of response headers, plus two synthetic policy
//! files.
//!
//! Three independent switches act on every response the proxy emits:
//!
//! - `--boostaler-cache` extends `Cache-Control` with stale-serving
//!   directives.
//! - `--cors-allow-all` adds permissive CORS headers and serves mocks for
//!   the two legacy cross-domain policy files.
//! - `--strip-security-headers` removes CSP and related security headers.
//!
//! Only the response head is rewritten, except for the policy mocks,
//! which replace the body as well. Nothing here can reach inside a
//! `CONNECT` tunnel: after the proxy answers `200`, the remaining bytes
//! are the client's encrypted session.

use http::header::{
    HeaderMap, HeaderName, HeaderValue, CACHE_CONTROL, CONTENT_LENGTH,
    CONTENT_TYPE, TRANSFER_ENCODING,
};
use http::{Method, Response};
use hyper::Uri;

use crate::proxy_http_forwarder::{full_body, BoxBody, StatusCode};

/// One day, in seconds: how long a stale response may be served while it
/// revalidates.
const STALE_WHILE_REVALIDATE_SECONDS: u64 = 86_400;

/// Three days, in seconds: how long a stale response may be served when
/// the origin returns an error.
const STALE_IF_ERROR_SECONDS: u64 = 259_200;

/// Methods advertised by the permissive CORS profile.
const CORS_ALLOWED_METHODS: &str =
    "CONNECT,DELETE,GET,HEAD,OPTIONS,PATCH,PING,POST,PUT,TRACE";

/// Request headers the permissive profile allows and exposes.
const CORS_ALLOWED_HEADERS: &str = "Accept-Ranges,Authorization,\
Content-Disposition,Content-Encoding,Content-Length,Content-Range,\
Content-Type,ETag,Last-Modified,Range,X-Requested-With";

/// Security headers removed by `--strip-security-headers`.
///
/// Matching is case-insensitive because `HeaderMap` normalizes names.
const STRIPPED_SECURITY_HEADERS: &[&str] = &[
    "p3p",
    "alt-svc",
    "strict-transport-security",
    "x-frame-options",
    "x-xss-protection",
    "x-content-type-options",
    "content-security-policy",
    "referrer-policy",
    "public-key-pins-report-only",
];

/// Flash cross-site policy, served for any path ending in
/// `CrossDomain.xml`.
const CROSS_DOMAIN_XML: &str = r#"<?xml version="1.0"?>
<!DOCTYPE cross-domain-policy SYSTEM "http://www.adobe.com/xml/dtds/cross-domain-policy.dtd">
<cross-domain-policy>
  <site-control permitted-cross-domain-policies="all"/>
  <allow-access-from domain="*" to-ports="*" secure="false"/>
  <allow-http-request-headers-from domain="*" headers="*" secure="false"/>
</cross-domain-policy>
"#;

/// Silverlight cross-site policy, served for any path ending in
/// `ClientAccessPolicy.xml`.
const CLIENT_ACCESS_POLICY_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<access-policy>
  <cross-domain-access>
    <policy>
      <allow-from http-request-headers="*">
        <domain uri="*"/>
      </allow-from>
      <grant-to>
        <resource path="/" include-subpaths="true"/>
      </grant-to>
    </policy>
  </cross-domain-access>
</access-policy>
"#;

/// Response-rewrite switches for one proxy process.
///
/// The value is built once from the command line and copied into each
/// connection handler.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ResponsePolicy {
    /// Extend `Cache-Control` with stale-serving directives.
    pub(crate) boostaler_cache: bool,
    /// Add permissive CORS headers and mock cross-domain policy files.
    pub(crate) cors_allow_all: bool,
    /// Remove CSP and related security headers.
    pub(crate) strip_security_headers: bool,
}

impl ResponsePolicy {
    /// Applies every enabled rewrite to an outgoing response head.
    ///
    /// Order does not matter because the three features touch different
    /// header names, but running cache/CORS before stripping means a
    /// stripped header can never be re-added by accident.
    pub(crate) fn apply(&self, response: &mut Response<BoxBody>) {
        if self.boostaler_cache {
            apply_stale_cache_control(response.headers_mut());
        }
        if self.cors_allow_all {
            apply_permissive_cors(response.headers_mut());
        }
        if self.strip_security_headers {
            strip_security_headers(response.headers_mut());
        }
    }
}

/// Removes existing stale directives, then appends the Boostaler pair.
///
/// All `Cache-Control` headers are merged into one value so that a
/// pre-existing `stale-while-revalidate` cannot survive in a second
/// header field.
fn apply_stale_cache_control(headers: &mut HeaderMap) {
    let mut directives: Vec<String> = Vec::new();
    for value in headers.get_all(CACHE_CONTROL).iter() {
        let Ok(text) = value.to_str() else {
            continue;
        };
        for directive in text.split(',') {
            let directive = directive.trim();
            if directive.is_empty() {
                continue;
            }
            let lower = directive.to_ascii_lowercase();
            if lower.starts_with("stale-while-revalidate")
                || lower.starts_with("stale-if-error")
            {
                continue;
            }
            directives.push(directive.to_string());
        }
    }
    directives.push(format!(
        "stale-while-revalidate={}",
        STALE_WHILE_REVALIDATE_SECONDS
    ));
    directives.push(format!(
        "stale-if-error={}",
        STALE_IF_ERROR_SECONDS
    ));
    if let Ok(value) = HeaderValue::from_str(&directives.join(", ")) {
        headers.insert(CACHE_CONTROL, value);
    }
}

/// Replaces every CORS-related header with the permissive value.
fn apply_permissive_cors(headers: &mut HeaderMap) {
    set_header(headers, "access-control-allow-methods", CORS_ALLOWED_METHODS);
    set_header(headers, "access-control-allow-origin", "*");
    set_header(headers, "access-control-allow-headers", CORS_ALLOWED_HEADERS);
    set_header(headers, "access-control-expose-headers", CORS_ALLOWED_HEADERS);
    set_header(headers, "access-control-allow-credentials", "true");
    set_header(headers, "timing-allow-origin", "*");
    set_header(headers, "x-permitted-cross-domain-policies", "all");
}

/// Removes any existing values for a header, then sets the requested one.
fn set_header(
    headers: &mut HeaderMap,
    name: &'static str,
    value: &'static str,
) {
    headers.remove(name);
    headers.insert(HeaderName::from_static(name), HeaderValue::from_static(value));
}

/// Drops the headers that restrict framing, scripting and transport
/// upgrades.
fn strip_security_headers(headers: &mut HeaderMap) {
    for name in STRIPPED_SECURITY_HEADERS {
        headers.remove(*name);
    }
}

/// Returns the mock body when the request path ends in one of the two
/// legacy cross-domain policy file names.
///
/// The comparison ignores ASCII case, so `/foo/CROSSDOMAIN.XML` matches
/// too. `CONNECT` requests have no path to match and are skipped.
pub(crate) fn cross_domain_mock(
    method: &Method,
    uri: &Uri,
) -> Option<&'static str> {
    if method == Method::CONNECT {
        return None;
    }
    let file = uri.path().rsplit('/').next().unwrap_or_default();
    if file.eq_ignore_ascii_case("crossdomain.xml") {
        Some(CROSS_DOMAIN_XML)
    } else if file.eq_ignore_ascii_case("clientaccesspolicy.xml") {
        Some(CLIENT_ACCESS_POLICY_XML)
    } else {
        None
    }
}

/// Builds the synthetic `200 OK` that stands in for a policy file.
///
/// `Content-Length` is computed from the chosen body rather than copied
/// from any upstream response, and `Transfer-Encoding` is dropped so the
/// framing is unambiguous.
pub(crate) fn cross_domain_response(body: &'static str) -> Response<BoxBody> {
    let length = HeaderValue::from_str(&body.len().to_string())
        .expect("body length is valid header text");
    let mut response = Response::builder()
        .status(StatusCode::OK)
        .header(CONTENT_TYPE, "text/x-cross-domain-policy")
        .header(CONTENT_LENGTH, length)
        .body(full_body(body))
        .expect("valid synthetic cross-domain response");
    response.headers_mut().remove(TRANSFER_ENCODING);
    response
}
