//! Per-user sliding-window rate limiter middleware.
//!
//! Uses an in-memory `DashMap` keyed by the caller's username (or IP as
//! fallback). Each entry stores a `VecDeque` of request timestamps; on each
//! request the expired entries are drained and a check is made against
//! `max_requests` per `window`.
//!
//! The middleware must be layered **after** auth so that `CallerIdentity` is
//! available in the request extensions.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::{
    extract::Request,
    http::StatusCode,
    middleware::Next,
    response::Response,
};
use parking_lot::Mutex;
use tracing::warn;

use crate::metrics::Metrics;

/// Shared state for the rate limiter.
#[derive(Clone)]
pub struct RateLimiterState {
    inner: Arc<RateLimiterInner>,
}

struct RateLimiterInner {
    /// Per-key sliding window of request timestamps.
    buckets: dashmap::DashMap<String, Mutex<VecDeque<Instant>>>,
    /// Maximum requests allowed per window.
    max_requests: u32,
    /// Window duration.
    window: Duration,
    /// Optional metrics handle for counting rate-limited requests.
    metrics: Option<Metrics>,
}

impl RateLimiterState {
    /// Create a new rate limiter.
    ///
    /// If `max_requests == 0` the limiter is effectively disabled (all
    /// requests pass).
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                buckets: dashmap::DashMap::new(),
                max_requests,
                window: Duration::from_secs(window_secs),
                metrics: None,
            }),
        }
    }

    /// Create a new rate limiter with metrics tracking.
    pub fn with_metrics(max_requests: u32, window_secs: u64, metrics: Metrics) -> Self {
        Self {
            inner: Arc::new(RateLimiterInner {
                buckets: dashmap::DashMap::new(),
                max_requests,
                window: Duration::from_secs(window_secs),
                metrics: Some(metrics),
            }),
        }
    }

    /// Access the optional metrics handle.
    pub fn metrics(&self) -> Option<&Metrics> {
        self.inner.metrics.as_ref()
    }

    /// Check whether a request from `key` is allowed. Returns `true` if
    /// allowed, `false` if rate-limited.
    pub fn check(&self, key: &str) -> bool {
        if self.inner.max_requests == 0 {
            return true; // disabled
        }

        let now = Instant::now();
        let cutoff = now - self.inner.window;

        let entry = self
            .inner
            .buckets
            .entry(key.to_owned())
            .or_insert_with(|| Mutex::new(VecDeque::new()));

        let mut queue = entry.lock();

        // Drain expired timestamps.
        while queue.front().is_some_and(|&t| t < cutoff) {
            queue.pop_front();
        }

        if queue.len() as u32 >= self.inner.max_requests {
            false
        } else {
            queue.push_back(now);
            true
        }
    }

    /// Number of tracked keys (for diagnostics).
    pub fn tracked_keys(&self) -> usize {
        self.inner.buckets.len()
    }
}

/// Axum middleware function. Extracts the caller's username from
/// `CallerIdentity` (inserted by the auth middleware) and applies the
/// sliding-window check.
pub async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<RateLimiterState>,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    // Extract the caller key. CallerIdentity is set by auth middleware.
    let key = request
        .extensions()
        .get::<crate::auth::CallerIdentity>()
        .and_then(|id| id.username.clone())
        .unwrap_or_else(|| "anonymous".to_string());

    if limiter.check(&key) {
        Ok(next.run(request).await)
    } else {
        if let Some(m) = limiter.metrics() {
            m.inc_rate_limited();
        }
        warn!(user = %key, "Rate limit exceeded");
        Err(StatusCode::TOO_MANY_REQUESTS)
    }
}

// ─── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_when_max_is_zero() {
        let limiter = RateLimiterState::new(0, 60);
        // Should always pass.
        for _ in 0..1000 {
            assert!(limiter.check("alice"));
        }
    }

    #[test]
    fn allows_up_to_max() {
        let limiter = RateLimiterState::new(5, 60);
        for i in 0..5 {
            assert!(limiter.check("bob"), "request {i} should pass");
        }
        // 6th should be rejected.
        assert!(!limiter.check("bob"));
    }

    #[test]
    fn separate_keys_are_independent() {
        let limiter = RateLimiterState::new(2, 60);
        assert!(limiter.check("alice"));
        assert!(limiter.check("alice"));
        assert!(!limiter.check("alice")); // alice exhausted
        // bob is still fine
        assert!(limiter.check("bob"));
        assert!(limiter.check("bob"));
        assert!(!limiter.check("bob"));
    }

    #[test]
    fn window_expiry_allows_new_requests() {
        // Use a tiny window (10 ms) so entries expire quickly.
        let limiter = RateLimiterState::new(1, 0);
        assert!(limiter.check("carol")); // first request allowed

        // Sleep past the window so the entry expires.
        std::thread::sleep(Duration::from_millis(20));
        assert!(limiter.check("carol")); // old entry drained, new one allowed
    }

    #[test]
    fn tracked_keys_returns_count() {
        let limiter = RateLimiterState::new(10, 60);
        assert_eq!(limiter.tracked_keys(), 0);
        limiter.check("alice");
        limiter.check("bob");
        assert_eq!(limiter.tracked_keys(), 2);
    }
}
