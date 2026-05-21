//! [`RecoveryProvider`]: a wrapper around an [`LlmProvider`] that recovers
//! from context-window overflow by compacting the request's message history
//! with a [`Compressor`] and retrying.
//!
//! Opt-in per kernel guideline G4 (no prebuilt layers as silent defaults):
//! the runtime only wraps a provider when the caller explicitly composes a
//! [`RecoveryProvider`]. The compressor itself is a plugin (G8/D14); this
//! wrapper depends only on the core [`Compressor`] trait, so the caller
//! injects whichever compressor they want (the default being
//! `pg-synapse-compaction`'s `DefaultCompressor`).
//!
//! Behavior:
//!   - [`LlmError::ContextLengthExceeded`] -> compact history, retry (bounded
//!     by `config.max_attempts`).
//!   - Any other error or success -> passed through unchanged.
//!   - Compaction that collapses nothing, or a compressor error -> the
//!     original overflow error is surfaced (no infinite loop, real problem
//!     not masked).
//!
//! Composes with [`crate::RetryProvider`]: wrap the provider in `Retry` for
//! transient faults and `Recovery` for overflow independently.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;

use crate::compression::Compressor;
use crate::error::LlmError;
use crate::llm::LlmProvider;
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse, Message, Role};

/// Configuration for overflow recovery.
#[derive(Debug, Clone)]
pub struct RecoveryConfig {
    /// Maximum compact-then-retry cycles (not counting the initial call).
    pub max_attempts: u32,
    /// Soft target as a fraction of the model's context limit. The remaining
    /// `1.0 - target_ratio` leaves headroom for the completion itself.
    pub target_ratio: f64,
}

impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            max_attempts: 1,
            target_ratio: 0.5,
        }
    }
}

/// Build the single replacement [`Message`] that stands in for the collapsed
/// span. Role `System` so the digest is treated as durable context rather
/// than a turn the model must respond to.
fn summary_message(template: Option<&Message>, summary: String) -> Message {
    let mut m = match template {
        Some(t) => Message {
            execution_id: t.execution_id,
            seq: 0,
            role: Role::System,
            content: None,
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: t.timestamp,
        },
        None => Message {
            execution_id: uuid::Uuid::nil(),
            seq: 0,
            role: Role::System,
            content: None,
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: chrono::Utc::now(),
        },
    };
    m.content = Some(summary);
    m
}

/// Rebuild a message list, dropping the collapsed indices and inserting a
/// single summary message at the position of the earliest collapsed message.
fn rebuild_messages(messages: &[Message], summary: &str, collapsed: &[u32]) -> Vec<Message> {
    let collapsed_set: std::collections::HashSet<u32> = collapsed.iter().copied().collect();
    let first_collapsed = collapsed.iter().copied().min();
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let mut inserted = false;
    for (i, m) in messages.iter().enumerate() {
        let idx = i as u32;
        if collapsed_set.contains(&idx) {
            if !inserted {
                let template = first_collapsed.map(|fc| &messages[fc as usize]);
                out.push(summary_message(template, summary.to_string()));
                inserted = true;
            }
            continue;
        }
        out.push(m.clone());
    }
    out
}

/// An [`LlmProvider`] wrapper that compacts history and retries on context
/// overflow.
///
/// Construct via [`RecoveryProvider::new`]. `stream` and `model_name` are
/// forwarded to the inner provider unchanged.
pub struct RecoveryProvider {
    inner: Arc<dyn LlmProvider>,
    compressor: Arc<dyn Compressor>,
    config: RecoveryConfig,
}

impl RecoveryProvider {
    /// Wrap `inner`, using `compressor` to shrink history on overflow.
    pub fn new(
        inner: Arc<dyn LlmProvider>,
        compressor: Arc<dyn Compressor>,
        config: RecoveryConfig,
    ) -> Self {
        Self {
            inner,
            compressor,
            config,
        }
    }
}

#[async_trait]
impl LlmProvider for RecoveryProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let mut req = req;
        let mut attempts = 0u32;

        loop {
            match self.inner.complete(req.clone()).await {
                Ok(resp) => return Ok(resp),
                Err(LlmError::ContextLengthExceeded {
                    model,
                    tokens,
                    limit,
                }) => {
                    let overflow = LlmError::ContextLengthExceeded {
                        model: model.clone(),
                        tokens,
                        limit,
                    };
                    if attempts >= self.config.max_attempts {
                        return Err(overflow);
                    }
                    attempts += 1;

                    // Derive the budget from the model's own reported limit:
                    // aim for `target_ratio` of the window, leaving the rest
                    // as completion headroom; never exceed the hard limit.
                    let hard = limit;
                    let target = ((limit as f64) * self.config.target_ratio) as u32;
                    let budget = crate::types::CompressionBudget {
                        target_tokens: target,
                        hard_limit_tokens: hard,
                    };

                    let compressed = match self.compressor.compress(&req.messages, budget).await {
                        Ok(c) => c,
                        Err(e) => {
                            tracing::warn!(
                                target: "pg_synapse_core::recovery",
                                error = %e,
                                "compaction failed; surfacing original overflow"
                            );
                            return Err(overflow);
                        }
                    };

                    if compressed.collapsed_indices.is_empty() {
                        // Nothing left to shrink: stop instead of looping.
                        tracing::warn!(
                            target: "pg_synapse_core::recovery",
                            "compaction collapsed nothing; cannot recover from overflow"
                        );
                        return Err(overflow);
                    }

                    let before = req.messages.len();
                    req.messages = rebuild_messages(
                        &req.messages,
                        &compressed.summary,
                        &compressed.collapsed_indices,
                    );
                    tracing::info!(
                        target: "pg_synapse_core::recovery",
                        attempt = attempts,
                        messages_before = before,
                        messages_after = req.messages.len(),
                        "compacted history; retrying after context overflow"
                    );
                }
                Err(other) => return Err(other),
            }
        }
    }

    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        self.inner.stream(req).await
    }

    fn model_name(&self) -> &str {
        self.inner.model_name()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compression::Compressor;
    use crate::error::CompressionError;
    use crate::testing::MockLlmProvider;
    use crate::types::{Compressed, CompressionBudget, Usage};
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn user_msg(content: &str) -> Message {
        Message {
            execution_id: uuid::Uuid::nil(),
            seq: 0,
            role: Role::User,
            content: Some(content.to_string()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: chrono::Utc::now(),
        }
    }

    fn req_with(n: usize) -> CompletionRequest {
        CompletionRequest {
            messages: (0..n).map(|i| user_msg(&format!("m{i}"))).collect(),
            ..Default::default()
        }
    }

    /// Collapses everything except the final message into one summary.
    struct CollapseAllButLast;
    #[async_trait]
    impl Compressor for CollapseAllButLast {
        async fn compress(
            &self,
            messages: &[Message],
            _budget: CompressionBudget,
        ) -> Result<Compressed, CompressionError> {
            if messages.len() <= 1 {
                return Ok(Compressed::default());
            }
            Ok(Compressed {
                summary: "DIGEST".into(),
                collapsed_indices: (0..messages.len() as u32 - 1).collect(),
            })
        }
    }

    /// Provider that errors with ContextLengthExceeded while the request has
    /// more than `threshold` messages, otherwise returns text. Records how
    /// many messages each call saw.
    struct OverflowUntilSmall {
        threshold: usize,
        seen: Mutex<Vec<usize>>,
        calls: AtomicUsize,
    }
    #[async_trait]
    impl LlmProvider for OverflowUntilSmall {
        async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen.lock().unwrap().push(req.messages.len());
            if req.messages.len() > self.threshold {
                return Err(LlmError::ContextLengthExceeded {
                    model: "m".into(),
                    tokens: 9999,
                    limit: 1000,
                });
            }
            Ok(CompletionResponse {
                content: Some("ok".into()),
                tool_calls: vec![],
                usage: Usage::default(),
                finish_reason: String::new(),
            })
        }
        async fn stream(
            &self,
            _req: CompletionRequest,
        ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
            unreachable!()
        }
        fn model_name(&self) -> &str {
            "m"
        }
    }

    #[tokio::test]
    async fn forwards_success_unchanged() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_text("hello");
        let rp = RecoveryProvider::new(
            mock,
            Arc::new(CollapseAllButLast),
            RecoveryConfig::default(),
        );
        let resp = rp.complete(req_with(3)).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("hello"));
    }

    #[tokio::test]
    async fn non_overflow_error_passes_through() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_error(LlmError::Auth("bad".into()));
        let rp = RecoveryProvider::new(
            mock,
            Arc::new(CollapseAllButLast),
            RecoveryConfig::default(),
        );
        let err = rp.complete(req_with(3)).await.unwrap_err();
        assert!(matches!(err, LlmError::Auth(_)));
    }

    #[tokio::test]
    async fn compacts_and_retries_on_overflow() {
        // CollapseAllButLast always leaves {summary + final} = 2 messages,
        // so threshold must be >= 2 for the retry to clear the window.
        let prov = Arc::new(OverflowUntilSmall {
            threshold: 2,
            seen: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        });
        let rp = RecoveryProvider::new(
            prov.clone(),
            Arc::new(CollapseAllButLast),
            RecoveryConfig::default(),
        );
        let resp = rp.complete(req_with(5)).await.unwrap();
        assert_eq!(resp.content.as_deref(), Some("ok"));
        let seen = prov.seen.lock().unwrap().clone();
        // First call saw 5 messages (overflow), retry saw the compacted set
        // (1 summary + 1 retained final = 2) and succeeded.
        assert_eq!(seen.first().copied(), Some(5));
        assert_eq!(seen.last().copied(), Some(2));
        assert!(
            seen.last().copied().unwrap() < 5,
            "retry must see a compacted (shorter) request, saw {seen:?}"
        );
    }

    #[tokio::test]
    async fn gives_up_after_max_attempts() {
        // threshold 0 => always overflows; recovery can never satisfy it.
        let prov = Arc::new(OverflowUntilSmall {
            threshold: 0,
            seen: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        });
        let rp = RecoveryProvider::new(
            prov.clone(),
            Arc::new(CollapseAllButLast),
            RecoveryConfig {
                max_attempts: 2,
                target_ratio: 0.5,
            },
        );
        let err = rp.complete(req_with(5)).await.unwrap_err();
        assert!(matches!(err, LlmError::ContextLengthExceeded { .. }));
        // initial + 2 recovery attempts = 3 calls, then give up.
        assert_eq!(prov.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn surfaces_overflow_when_compressor_collapses_nothing() {
        struct NoOpCompressor;
        #[async_trait]
        impl Compressor for NoOpCompressor {
            async fn compress(
                &self,
                _m: &[Message],
                _b: CompressionBudget,
            ) -> Result<Compressed, CompressionError> {
                Ok(Compressed::default())
            }
        }
        let prov = Arc::new(OverflowUntilSmall {
            threshold: 0,
            seen: Mutex::new(vec![]),
            calls: AtomicUsize::new(0),
        });
        let rp = RecoveryProvider::new(
            prov.clone(),
            Arc::new(NoOpCompressor),
            RecoveryConfig {
                max_attempts: 5,
                target_ratio: 0.5,
            },
        );
        let err = rp.complete(req_with(4)).await.unwrap_err();
        assert!(matches!(err, LlmError::ContextLengthExceeded { .. }));
        // No progress possible: stop after the first failed compaction
        // (initial call + nothing further), never loop to max_attempts.
        assert_eq!(prov.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn model_name_delegates_to_inner() {
        let mock = Arc::new(MockLlmProvider::new("gpt-test"));
        let rp = RecoveryProvider::new(
            mock,
            Arc::new(CollapseAllButLast),
            RecoveryConfig::default(),
        );
        assert_eq!(rp.model_name(), "gpt-test");
    }
}
