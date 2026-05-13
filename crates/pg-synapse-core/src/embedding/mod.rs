//! The [`EmbeddingProvider`] trait and its [`EmbeddingProviderFactory`].

use async_trait::async_trait;
use std::sync::Arc;

use crate::error::{EmbeddingError, ProviderError};
use crate::types::{EmbeddingProfileRow, EmbeddingVector};

/// A backend that turns text into dense embedding vectors.
///
/// `embed` takes a batch of `&str` references and returns one
/// [`EmbeddingVector`] per input, in the same order. All returned vectors must
/// share the [`EmbeddingProvider::dimension`] declared by the provider.
///
/// ## Example
///
/// ```
/// use pg_synapse_core::testing::MockEmbeddingProvider;
/// use pg_synapse_core::EmbeddingProvider;
///
/// # tokio_test::block_on(async {
/// let m = MockEmbeddingProvider::new("test", 4);
/// let v = m.embed(&["hello"]).await.unwrap();
/// assert_eq!(v.len(), 1);
/// assert_eq!(v[0].dimension(), 4);
/// # });
/// ```
#[async_trait]
pub trait EmbeddingProvider: Send + Sync {
    /// Embed a batch of texts.
    async fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbeddingError>;

    /// Vector dimension produced by this provider.
    fn dimension(&self) -> usize;

    /// Model name (for trace rows).
    fn model_name(&self) -> &str;
}

/// Factory that turns one [`EmbeddingProfileRow`] into an `Arc<dyn EmbeddingProvider>`.
pub trait EmbeddingProviderFactory: Send + Sync {
    /// Provider name this factory handles (e.g. `"openai"`, `"ort"`).
    fn provider_name(&self) -> &str;

    /// Build a live provider from one profile row.
    fn build(
        &self,
        profile: EmbeddingProfileRow,
    ) -> Result<Arc<dyn EmbeddingProvider>, ProviderError>;
}
