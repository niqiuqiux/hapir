use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use rand::Rng;

/// Async sleep for the given number of milliseconds.
pub async fn delay(ms: u64) {
    tokio::time::sleep(Duration::from_millis(ms)).await;
}

/// Calculate an exponential backoff delay with jitter.
///
/// `current_failure_count` - how many consecutive failures so far
/// `min_delay_ms` - minimum delay in milliseconds
/// `max_delay_ms` - maximum delay in milliseconds
/// `max_failure_count` - failure count at which delay is capped
pub fn exponential_backoff_delay(
    current_failure_count: u32,
    min_delay_ms: u64,
    max_delay_ms: u64,
    max_failure_count: u32,
) -> Duration {
    let capped = current_failure_count.min(max_failure_count);
    let range = max_delay_ms.saturating_sub(min_delay_ms);
    let max_ret = min_delay_ms as f64
        + (range as f64 / max_failure_count as f64) * capped as f64;
    let ms = rand::thread_rng().gen_range(0.0..=max_ret);
    Duration::from_millis(ms.round() as u64)
}

type RetryCallback = Box<dyn Fn(&anyhow::Error, u32, Duration) + Send + Sync>;

/// Options for `with_retry`.
pub struct RetryOptions<S>
where
    S: Fn(&anyhow::Error) -> bool,
{
    /// Maximum number of retry attempts. `None` means unlimited.
    pub max_attempts: Option<u32>,
    /// Minimum delay between retries in ms. Default: 1000.
    pub min_delay_ms: u64,
    /// Maximum delay between retries in ms. Default: 30000.
    pub max_delay_ms: u64,
    /// Predicate to decide if an error is retryable.
    pub should_retry: S,
    /// Optional callback before each retry.
    pub on_retry: Option<RetryCallback>,
}

impl Default for RetryOptions<fn(&anyhow::Error) -> bool> {
    fn default() -> Self {
        Self {
            max_attempts: None,
            min_delay_ms: 1000,
            max_delay_ms: 30000,
            should_retry: |_| true,
            on_retry: None,
        }
    }
}

/// Execute an async function with retry logic and exponential backoff.
///
/// Uses 2^n backoff with 0-30% jitter, capped at `max_delay_ms`.
pub async fn with_retry<T, F, Fut, S>(
    f: F,
    options: RetryOptions<S>,
) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T>>,
    S: Fn(&anyhow::Error) -> bool,
{
    let mut attempt: u32 = 0;

    loop {
        match f().await {
            Ok(val) => return Ok(val),
            Err(error) => {
                attempt += 1;

                if !(options.should_retry)(&error) {
                    return Err(error);
                }

                if let Some(max) = options.max_attempts
                    && attempt >= max
                {
                    return Err(error);
                }

                let exp_delay =
                    options.min_delay_ms as f64 * 2f64.powi((attempt - 1) as i32);
                let capped = exp_delay.min(options.max_delay_ms as f64);
                let jitter = rand::thread_rng().gen_range(0.0..=0.3) * capped;
                let next_delay =
                    Duration::from_millis((capped + jitter).round() as u64);

                if let Some(ref cb) = options.on_retry {
                    cb(&error, attempt, next_delay);
                }

                tokio::time::sleep(next_delay).await;
            }
        }
    }
}
