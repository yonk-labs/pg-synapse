//! [`RetryProvider`]: a wrapper around an [`LlmProvider`] that retries
//! transient errors with jittered exponential backoff.
//!
//! This is opt-in per kernel guideline G4 (no prebuilt layers as silent
//! defaults). The runtime only wraps a provider when the caller has explicitly
//! configured a [`RetryConfig`] on the [`RuntimeBuilder`].
//!
//! Transient errors (retried):
//!   - [`LlmError::RateLimited`] -- respects `retry_after_ms` when present
//!   - [`LlmError::Provider`] -- covers 5xx server errors
//!   - [`LlmError::Network`] -- DNS / TCP / TLS / HTTP transport failures
//!
//! Permanent errors (not retried):
//!   - [`LlmError::Auth`]
//!   - [`LlmError::ContextLengthExceeded`]
//!
//! [`RuntimeBuilder`]: crate::RuntimeBuilder

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::error::LlmError;
use crate::llm::LlmProvider;
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse};

/// Configuration for jittered exponential backoff.
#[derive(Debug, Clone)]
pub struct RetryConfig {
    /// Maximum number of retry attempts (not counting the initial call).
    pub max_retries: u32,
    /// Base delay in milliseconds for the first retry.
    pub base_delay_ms: u64,
    /// Upper bound on the computed delay in milliseconds.
    pub max_delay_ms: u64,
    /// Fraction of the computed delay used as the jitter window (0.0..1.0).
    /// A value of 0.25 means the final delay is `base +/- 25%`.
    pub jitter_fraction: f64,
}

impl Default for RetryConfig {
    fn default() -> Self {
        Self {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            jitter_fraction: 0.25,
        }
    }
}

/// Returns `true` for errors that are transient and worth retrying.
fn is_transient(err: &LlmError) -> bool {
    matches!(
        err,
        LlmError::RateLimited { .. } | LlmError::Provider { .. } | LlmError::Network(_)
    )
}

/// Compute the backoff delay for a given `attempt` (0-indexed).
///
/// Formula: `min(base_delay_ms * 2^attempt, max_delay_ms)`, then jitter
/// applied as `delay * (1.0 + uniform(-jitter_fraction, +jitter_fraction))`.
///
/// `jitter_seed` is a value in `0.0..1.0` used to deterministically compute
/// jitter in tests (production passes a cheap pseudo-random value).
fn compute_delay(config: &RetryConfig, attempt: u32, jitter_seed: f64) -> u64 {
    let shift = attempt.min(63) as u64;
    let exp_delay = config.base_delay_ms.saturating_mul(1u64 << shift);
    let capped = exp_delay.min(config.max_delay_ms);

    // Map jitter_seed from [0, 1) to [-jitter_fraction, +jitter_fraction).
    let jitter_multiplier = 1.0 + config.jitter_fraction * (2.0 * jitter_seed - 1.0);
    let jittered = (capped as f64 * jitter_multiplier).round() as u64;

    // Ensure we never go below 1ms or above max_delay_ms.
    jittered.max(1).min(config.max_delay_ms)
}

/// Extract `retry_after_ms` from a `RateLimited` error, if present.
fn retry_after_hint(err: &LlmError) -> Option<u64> {
    if let LlmError::RateLimited {
        retry_after_ms: Some(ms),
        ..
    } = err
    {
        Some(*ms)
    } else {
        None
    }
}

/// Cheap pseudo-random `f64` in `[0, 1)` seeded from the current time.
///
/// Not cryptographically secure. Only used for jitter, where uniformity
/// across retries matters more than unpredictability.
fn cheap_random_f64() -> f64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Mix with a thread-local counter to avoid identical seeds within one ms.
    std::thread_local! {
        static CTR: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let ctr = CTR.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v
    });
    let mixed = nanos.wrapping_mul(2654435761).wrapping_add(ctr);
    (mixed as f64) / (u32::MAX as f64)
}

/// An [`LlmProvider`] wrapper that retries transient errors with jittered
/// exponential backoff.
///
/// Construct via [`RetryProvider::new`]. The wrapper delegates `complete` to
/// the inner provider, retrying on transient errors up to `config.max_retries`
/// times. Streaming (`stream`) and `model_name` are forwarded without retry
/// logic.
pub struct RetryProvider {
    inner: Arc<dyn LlmProvider>,
    config: RetryConfig,
}

impl RetryProvider {
    /// Wrap `inner` with the given retry configuration.
    pub fn new(inner: Arc<dyn LlmProvider>, config: RetryConfig) -> Self {
        Self { inner, config }
    }
}

#[async_trait]
impl LlmProvider for RetryProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let mut last_err: Option<LlmError> = None;

        for attempt in 0..=self.config.max_retries {
            // Clone the request for each attempt. CompletionRequest is Clone.
            let result = self.inner.complete(req.clone()).await;

            match result {
                Ok(resp) => return Ok(resp),
                Err(err) => {
                    if !is_transient(&err) || attempt == self.config.max_retries {
                        return Err(err);
                    }

                    // Determine delay: prefer Retry-After hint, fall back to
                    // exponential backoff with jitter.
                    let delay_ms = match retry_after_hint(&err) {
                        Some(hint) => hint,
                        None => compute_delay(&self.config, attempt, cheap_random_f64()),
                    };

                    tracing::warn!(
                        target: "pg_synapse_core::retry",
                        attempt = attempt + 1,
                        max = self.config.max_retries,
                        delay_ms = delay_ms,
                        error = %err,
                        "retrying transient LLM error"
                    );

                    tokio::time::sleep(std::time::Duration::from_millis(delay_ms)).await;
                    last_err = Some(err);
                }
            }
        }

        // Should be unreachable: the loop returns on success or final failure.
        Err(last_err.unwrap_or_else(|| LlmError::Provider {
            provider: "retry".into(),
            reason: "retry loop exited without result".into(),
        }))
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        // Streaming is forwarded without retry. Retrying a stream mid-flight
        // is a different problem (reconnect + dedup) outside v0.1 scope.
        self.inner.stream(req).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::MockLlmProvider;
    use crate::types::CompletionRequest;

    /// Helper: build a RetryConfig with zero delays for fast tests.
    fn fast_config(max_retries: u32) -> RetryConfig {
        RetryConfig {
            max_retries,
            base_delay_ms: 1,
            max_delay_ms: 1,
            jitter_fraction: 0.0,
        }
    }

    #[tokio::test]
    async fn transient_error_retries_and_succeeds() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        // First call: transient error. Second call: success.
        mock.push_error(LlmError::RateLimited {
            provider: "openai".into(),
            retry_after_ms: None,
        });
        mock.push_text("ok");

        let retry = RetryProvider::new(mock.clone(), fast_config(3));
        let resp = retry.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("ok"));
        assert_eq!(mock.queued(), 0, "both responses should have been consumed");
    }

    #[tokio::test]
    async fn permanent_error_does_not_retry() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        mock.push_error(LlmError::Auth("bad key".into()));
        // This text should never be reached.
        mock.push_text("should not reach");

        let retry = RetryProvider::new(mock.clone(), fast_config(3));
        let err = retry
            .complete(CompletionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)));
        assert_eq!(mock.queued(), 1, "second response should remain unconsumed");
    }

    #[tokio::test]
    async fn context_length_exceeded_does_not_retry() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        mock.push_error(LlmError::ContextLengthExceeded {
            model: "gpt-4".into(),
            tokens: 200_000,
            limit: 128_000,
        });
        mock.push_text("should not reach");

        let retry = RetryProvider::new(mock.clone(), fast_config(3));
        let err = retry
            .complete(CompletionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::ContextLengthExceeded { .. }));
        assert_eq!(mock.queued(), 1);
    }

    #[tokio::test]
    async fn max_retries_exhausted_returns_last_error() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        // Push 4 transient errors: 1 initial + 3 retries = 4 calls total.
        for _ in 0..4 {
            mock.push_error(LlmError::Network("connection reset".into()));
        }

        let retry = RetryProvider::new(mock.clone(), fast_config(3));
        let err = retry
            .complete(CompletionRequest::default())
            .await
            .unwrap_err();
        assert!(matches!(err, LlmError::Network(_)));
        assert_eq!(mock.queued(), 0, "all 4 error responses should be consumed");
    }

    #[tokio::test]
    async fn retry_after_hint_is_respected() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        mock.push_error(LlmError::RateLimited {
            provider: "openai".into(),
            retry_after_ms: Some(50),
        });
        mock.push_text("ok");

        let config = RetryConfig {
            max_retries: 3,
            base_delay_ms: 1,
            max_delay_ms: 1,
            jitter_fraction: 0.0,
        };
        let retry = RetryProvider::new(mock, config);

        let start = tokio::time::Instant::now();
        let resp = retry.complete(CompletionRequest::default()).await.unwrap();
        let elapsed = start.elapsed();

        assert_eq!(resp.content.as_deref(), Some("ok"));
        // The retry should have waited at least 50ms (the Retry-After hint).
        assert!(
            elapsed.as_millis() >= 45,
            "expected at least ~50ms delay from Retry-After, got {}ms",
            elapsed.as_millis()
        );
    }

    #[tokio::test]
    async fn network_error_is_transient_and_retried() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        mock.push_error(LlmError::Network("DNS failed".into()));
        mock.push_text("recovered");

        let retry = RetryProvider::new(mock.clone(), fast_config(2));
        let resp = retry.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("recovered"));
    }

    #[tokio::test]
    async fn provider_error_is_transient_and_retried() {
        let mock = Arc::new(MockLlmProvider::new("test"));
        mock.push_error(LlmError::Provider {
            provider: "openai".into(),
            reason: "HTTP 500: internal server error".into(),
        });
        mock.push_text("recovered");

        let retry = RetryProvider::new(mock.clone(), fast_config(2));
        let resp = retry.complete(CompletionRequest::default()).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("recovered"));
    }

    #[test]
    fn compute_delay_exponential_backoff() {
        let config = RetryConfig {
            max_retries: 5,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            jitter_fraction: 0.0, // no jitter for deterministic test
        };
        // seed doesn't matter with jitter_fraction=0.0
        assert_eq!(compute_delay(&config, 0, 0.5), 1000); // 1000 * 2^0
        assert_eq!(compute_delay(&config, 1, 0.5), 2000); // 1000 * 2^1
        assert_eq!(compute_delay(&config, 2, 0.5), 4000); // 1000 * 2^2
        assert_eq!(compute_delay(&config, 3, 0.5), 8000); // 1000 * 2^3
        assert_eq!(compute_delay(&config, 4, 0.5), 16000); // 1000 * 2^4
        assert_eq!(compute_delay(&config, 5, 0.5), 30_000); // capped at max
    }

    #[test]
    fn compute_delay_with_jitter_stays_in_bounds() {
        let config = RetryConfig {
            max_retries: 3,
            base_delay_ms: 1000,
            max_delay_ms: 30_000,
            jitter_fraction: 0.25,
        };
        // seed=0.0 -> jitter_multiplier = 1.0 + 0.25 * (0.0 - 1.0) = 0.75
        let low = compute_delay(&config, 0, 0.0);
        assert_eq!(low, 750); // 1000 * 0.75

        // seed=1.0 -> jitter_multiplier = 1.0 + 0.25 * (2.0 - 1.0) = 1.25
        let high = compute_delay(&config, 0, 1.0);
        assert_eq!(high, 1250); // 1000 * 1.25

        // seed=0.5 -> jitter_multiplier = 1.0 + 0.25 * (1.0 - 1.0) = 1.0
        let mid = compute_delay(&config, 0, 0.5);
        assert_eq!(mid, 1000);
    }

    #[test]
    fn model_name_delegates_to_inner() {
        let mock = Arc::new(MockLlmProvider::new("gpt-test"));
        let retry = RetryProvider::new(mock, RetryConfig::default());
        assert_eq!(retry.model_name(), "gpt-test");
    }
}
