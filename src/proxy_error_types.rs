//! Typed errors that carry the HTTP status they should become.
//!
//! Anything not represented here is treated as an upstream failure
//! (`502`). Adding a variant is how a new failure class gets its own
//! status without string matching in the service layer.

use std::time::Duration;

/// Errors that the proxy maps to a specific HTTP status code.
///
/// Keeping these typed instead of flattening everything into `anyhow`
/// lets the response layer distinguish a client mistake (`400`) from an
/// upstream timeout (`504`) without inspecting error strings.
#[derive(Debug, thiserror::Error)]
pub(crate) enum ProxyError {
    /// The client sent a request the proxy cannot handle.
    #[error("{0}")]
    BadRequest(String),
    /// An upstream operation exceeded its deadline.
    #[error("{phase} timed out after {duration:?}")]
    Timeout {
        phase: &'static str,
        duration: Duration,
    },
}
