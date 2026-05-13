//! The [`LlmProvider`] trait and [`LlmProviderFactory`] for building one from a
//! profile row.

use async_trait::async_trait;
use futures::stream::BoxStream;
use std::sync::Arc;

use crate::error::{LlmError, ProviderError};
use crate::types::{CompletionChunk, CompletionRequest, CompletionResponse, LlmProfileRow};

/// A backend that turns a [`CompletionRequest`] into a [`CompletionResponse`].
///
/// Implementations must be `Send + Sync` and are typically held as
/// `Arc<dyn LlmProvider>` so the same provider can serve many concurrent
/// executions.
///
/// ## Example
///
/// ```
/// use pg_synapse_core::testing::MockLlmProvider;
/// use pg_synapse_core::types::CompletionRequest;
/// use pg_synapse_core::LlmProvider;
///
/// # tokio_test::block_on(async {
/// let mock = MockLlmProvider::new("test");
/// mock.push_text("hi");
/// let resp = mock.complete(CompletionRequest::default()).await.unwrap();
/// assert_eq!(resp.content.as_deref(), Some("hi"));
/// # });
/// ```
#[async_trait]
pub trait LlmProvider: Send + Sync {
    /// One-shot completion.
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError>;

    /// Streaming completion. Returns a boxed stream of chunks; the final
    /// chunk's `finish_reason` is set.
    async fn stream(
        &self,
        req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError>;

    /// Default model name for this provider (used for trace rows when the
    /// request leaves `model` unset).
    fn model_name(&self) -> &str;
}

/// Factory that turns one [`LlmProfileRow`] into an `Arc<dyn LlmProvider>`.
///
/// One factory per provider. Plugin crates implement this and register it on
/// the host's [`crate::Registry`] at startup.
pub trait LlmProviderFactory: Send + Sync {
    /// Provider name this factory handles (e.g. `"openai"`).
    fn provider_name(&self) -> &str;

    /// Build a live provider from one profile row.
    fn build(&self, profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError>;
}
