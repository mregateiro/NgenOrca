//! Request-ID middleware.
//!
//! Generates a unique `x-request-id` for every inbound request (or propagates
//! one supplied by the client / reverse proxy).  The ID is:
//!
//! 1. Injected into the current `tracing` span so that all log lines within
//!    the request carry the same correlation ID.
//! 2. Returned in the response as the `x-request-id` header.

use axum::{
    extract::Request,
    http::HeaderValue,
    middleware::Next,
    response::Response,
};
use tracing::Span;
use uuid::Uuid;

const REQUEST_ID_HEADER: &str = "x-request-id";

/// Middleware that assigns (or propagates) a unique request ID.
pub async fn request_id_middleware(request: Request, next: Next) -> Response {
    // Re-use an incoming request ID if the caller (or reverse proxy) supplied one.
    let request_id = request
        .headers()
        .get(REQUEST_ID_HEADER)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string())
        .unwrap_or_else(|| Uuid::now_v7().to_string());

    // Record the ID on the current tracing span so every log line correlates.
    Span::current().record("request_id", request_id.as_str());
    tracing::info!(request_id = %request_id, "request");

    let mut response = next.run(request).await;

    // Attach the request ID to the response so callers can correlate.
    if let Ok(val) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, val);
    }

    response
}
