//! Mock provider factories: thin adapters that hand back a pre-built
//! [`MockLlmProvider`] or [`MockEmbeddingProvider`] when the runtime asks the
//! corresponding factory to build a provider from a profile row.
//!
//! Tests use these to register a scripted provider against an arbitrary
//! `provider` string (typically `"mock"`), then drop a matching profile row
//! into a [`crate::runtime::test_utils::MockProfileSource`].
//!
//! These factories also implement [`Plugin`], so they can be installed via
//! [`crate::RuntimeBuilder::with_plugin`].

use std::sync::Arc;

use crate::embedding::{EmbeddingProvider, EmbeddingProviderFactory};
use crate::error::ProviderError;
use crate::llm::{LlmProvider, LlmProviderFactory};
use crate::plugin::{Plugin, Registry};
use crate::testing::{MockEmbeddingProvider, MockLlmProvider};
use crate::types::{EmbeddingProfileRow, LlmProfileRow};

/// LLM factory that hands back a fixed [`MockLlmProvider`] instance regardless
/// of the profile passed to [`Self::build`].
pub struct MockLlmFactory {
    provider_name: String,
    provider: Arc<MockLlmProvider>,
}

impl MockLlmFactory {
    /// Construct a factory that registers under `provider_name` and hands
    /// out `provider` on every call to `build`.
    pub fn new(provider_name: impl Into<String>, provider: Arc<MockLlmProvider>) -> Self {
        Self {
            provider_name: provider_name.into(),
            provider,
        }
    }
}

impl LlmProviderFactory for MockLlmFactory {
    fn provider_name(&self) -> &str {
        &self.provider_name
    }

    fn build(&self, _profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError> {
        Ok(self.provider.clone() as Arc<dyn LlmProvider>)
    }
}

impl Plugin for MockLlmFactory {
    fn name(&self) -> &str {
        "mock-llm"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn register(self, registry: &mut Registry) {
        registry.llm_factories.add(Arc::new(self));
    }
}

/// Embedding factory that hands back a fixed [`MockEmbeddingProvider`]
/// regardless of the profile passed to [`Self::build`].
pub struct MockEmbeddingFactory {
    provider_name: String,
    provider: Arc<MockEmbeddingProvider>,
}

impl MockEmbeddingFactory {
    /// Construct a factory that registers under `provider_name`.
    pub fn new(provider_name: impl Into<String>, provider: Arc<MockEmbeddingProvider>) -> Self {
        Self {
            provider_name: provider_name.into(),
            provider,
        }
    }
}

impl EmbeddingProviderFactory for MockEmbeddingFactory {
    fn provider_name(&self) -> &str {
        &self.provider_name
    }

    fn build(
        &self,
        _profile: EmbeddingProfileRow,
    ) -> Result<Arc<dyn EmbeddingProvider>, ProviderError> {
        Ok(self.provider.clone() as Arc<dyn EmbeddingProvider>)
    }
}

impl Plugin for MockEmbeddingFactory {
    fn name(&self) -> &str {
        "mock-embedding"
    }
    fn version(&self) -> &str {
        "0.1.0"
    }
    fn register(self, registry: &mut Registry) {
        registry.embedding_factories.add(Arc::new(self));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn mock_llm_factory_hands_back_provider() {
        let mock = Arc::new(MockLlmProvider::new("m"));
        mock.push_text("hi");
        let factory = MockLlmFactory::new("mock", mock);
        assert_eq!(factory.provider_name(), "mock");
        let provider = factory.build(LlmProfileRow::default()).unwrap();
        let resp = provider
            .complete(crate::types::CompletionRequest::default())
            .await
            .unwrap();
        assert_eq!(resp.content.as_deref(), Some("hi"));
    }

    #[tokio::test]
    async fn mock_embedding_factory_hands_back_provider() {
        let mock = Arc::new(MockEmbeddingProvider::new("e", 4));
        let factory = MockEmbeddingFactory::new("mock", mock);
        assert_eq!(factory.provider_name(), "mock");
        let provider = factory.build(EmbeddingProfileRow::default()).unwrap();
        let v = provider.embed(&["hello"]).await.unwrap();
        assert_eq!(v.len(), 1);
        assert_eq!(v[0].dimension(), 4);
    }

    #[test]
    fn mock_llm_factory_registers_as_plugin() {
        let mut r = Registry::new();
        let factory = MockLlmFactory::new("mock", Arc::new(MockLlmProvider::new("m")));
        factory.register(&mut r);
        assert!(r.llm_factories.contains("mock"));
    }

    #[test]
    fn mock_embedding_factory_registers_as_plugin() {
        let mut r = Registry::new();
        let factory =
            MockEmbeddingFactory::new("mock", Arc::new(MockEmbeddingProvider::new("e", 4)));
        factory.register(&mut r);
        assert!(r.embedding_factories.contains("mock"));
    }
}
