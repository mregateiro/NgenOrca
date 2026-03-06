//! LLM Model Provider implementations.
//!
//! Each provider translates [`ChatCompletionRequest`] into the appropriate
//! HTTP calls and maps responses back to [`ChatCompletionResponse`].

pub mod anthropic;
pub mod ollama;
pub mod openai_compat;
pub mod registry;
pub mod retry;

pub use registry::ProviderRegistry;
pub use retry::{retry_with_backoff, RetryConfig};

use ngenorca_core::Error;

/// Map an HTTP status code and response body to the appropriate error variant.
///
/// This allows the retry layer to distinguish transient errors (rate limits,
/// server errors) from permanent ones (auth, not found, bad request).
pub fn map_provider_http_error(provider: &str, status: reqwest::StatusCode, body: String) -> Error {
    match status.as_u16() {
        429 => {
            // Try to parse Retry-After header value from body (some providers embed it).
            Error::RateLimited(None)
        }
        500 | 502 | 503 | 504 => {
            Error::ProviderUnavailable(format!("{provider} HTTP {status}: {body}"))
        }
        401 | 403 => {
            Error::Unauthorized(format!("{provider} HTTP {status}: {body}"))
        }
        _ => {
            Error::Gateway(format!("{provider} HTTP {status}: {body}"))
        }
    }
}

/// Map a reqwest transport error to the appropriate error variant.
pub fn map_provider_transport_error(provider: &str, err: reqwest::Error) -> Error {
    if err.is_timeout() {
        Error::Timeout(std::time::Duration::from_secs(30))
    } else if err.is_connect() {
        Error::ProviderUnavailable(format!("{provider} connection failed: {err}"))
    } else {
        Error::Gateway(format!("{provider}: {err}"))
    }
}
