//! Retry utilities for transient provider errors.

use ngenorca_core::{Error, Result};
use std::future::Future;
use std::time::Duration;
use tracing::warn;

/// Default maximum retry attempts.
const DEFAULT_MAX_RETRIES: u32 = 3;

/// Initial backoff delay between retries.
const INITIAL_BACKOFF: Duration = Duration::from_millis(500);

/// Maximum backoff cap.
const MAX_BACKOFF: Duration = Duration::from_secs(10);

/// Retry configuration.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (0 = no retries, just the initial call).
    pub max_retries: u32,
    /// Initial backoff duration (doubled on each retry).
    pub initial_backoff: Duration,
    /// Maximum backoff duration cap.
    pub max_backoff: Duration,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: DEFAULT_MAX_RETRIES,
            initial_backoff: INITIAL_BACKOFF,
            max_backoff: MAX_BACKOFF,
        }
    }
}

/// Execute an async operation with exponential backoff retries on transient errors.
///
/// The `operation` closure is called once initially and up to `config.max_retries`
/// additional times if it returns a transient error (as determined by `Error::is_transient()`).
///
/// Non-transient errors are returned immediately without retrying.
pub async fn retry_with_backoff<F, Fut, T>(
    config: &RetryConfig,
    label: &str,
    mut operation: F,
) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T>>,
{
    let mut backoff = config.initial_backoff;
    let mut attempt = 0u32;

    loop {
        match operation().await {
            Ok(val) => return Ok(val),
            Err(e) if e.is_transient() && attempt < config.max_retries => {
                attempt += 1;

                // If it's a rate-limit error with a Retry-After hint, use that.
                let wait = if let Error::RateLimited(Some(retry_after)) = &e {
                    (*retry_after).min(config.max_backoff)
                } else {
                    backoff
                };

                warn!(
                    %label,
                    attempt,
                    max = config.max_retries,
                    backoff_ms = wait.as_millis() as u64,
                    error = %e,
                    "Transient error, retrying"
                );

                tokio::time::sleep(wait).await;
                backoff = (backoff * 2).min(config.max_backoff);
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn succeeds_on_first_try() {
        let config = RetryConfig::default();
        let result = retry_with_backoff(&config, "test", || async { Ok(42) }).await;
        assert_eq!(result.unwrap(), 42);
    }

    #[tokio::test]
    async fn retries_on_transient_error() {
        let attempts = AtomicU32::new(0);
        let config = RetryConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };

        let result = retry_with_backoff(&config, "test", || {
            let count = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if count < 2 {
                    Err(Error::ProviderUnavailable("down".into()))
                } else {
                    Ok("success")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "success");
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // initial + 2 retries
    }

    #[tokio::test]
    async fn does_not_retry_permanent_error() {
        let attempts = AtomicU32::new(0);
        let config = RetryConfig {
            max_retries: 3,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };

        let result: Result<i32> = retry_with_backoff(&config, "test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(Error::Config("bad".into())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1); // no retries
    }

    #[tokio::test]
    async fn gives_up_after_max_retries() {
        let attempts = AtomicU32::new(0);
        let config = RetryConfig {
            max_retries: 2,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };

        let result: Result<i32> = retry_with_backoff(&config, "test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(Error::ProviderUnavailable("always down".into())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3); // initial + 2 retries
    }

    #[tokio::test]
    async fn zero_max_retries_means_no_retry() {
        let attempts = AtomicU32::new(0);
        let config = RetryConfig {
            max_retries: 0,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_millis(10),
        };

        let result: Result<i32> = retry_with_backoff(&config, "test", || {
            attempts.fetch_add(1, Ordering::SeqCst);
            async { Err(Error::ProviderUnavailable("down".into())) }
        })
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn respects_rate_limit_retry_after() {
        let attempts = AtomicU32::new(0);
        let config = RetryConfig {
            max_retries: 1,
            initial_backoff: Duration::from_millis(1),
            max_backoff: Duration::from_secs(30),
        };

        let start = tokio::time::Instant::now();
        let result = retry_with_backoff(&config, "test", || {
            let count = attempts.fetch_add(1, Ordering::SeqCst);
            async move {
                if count == 0 {
                    Err(Error::RateLimited(Some(Duration::from_millis(50))))
                } else {
                    Ok("ok")
                }
            }
        })
        .await;

        assert_eq!(result.unwrap(), "ok");
        // Should have waited ~50ms (the Retry-After duration), not 1ms (initial_backoff).
        assert!(start.elapsed() >= Duration::from_millis(40));
    }
}
