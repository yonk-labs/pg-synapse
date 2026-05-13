//! Deterministic [`crate::EmbeddingProvider`] for tests.

use async_trait::async_trait;

use crate::embedding::EmbeddingProvider;
use crate::error::EmbeddingError;
use crate::types::EmbeddingVector;

/// In-memory embedding provider that returns a fixed-pattern vector for every
/// input. Vector length equals the configured `dimension`.
///
/// The pattern is `[0.1, 0.2, 0.3, ...]` repeating modulo 10, divided by 10 at
/// each step. The exact values are stable but should not be treated as
/// semantically meaningful; tests should assert on shape (length, count) only.
///
/// ## Example
///
/// ```
/// use pg_synapse_core::testing::MockEmbeddingProvider;
/// use pg_synapse_core::EmbeddingProvider;
///
/// # tokio_test::block_on(async {
/// let m = MockEmbeddingProvider::new("test", 4);
/// let v = m.embed(&["alpha", "beta"]).await.unwrap();
/// assert_eq!(v.len(), 2);
/// assert!(v.iter().all(|e| e.dimension() == 4));
/// # });
/// ```
pub struct MockEmbeddingProvider {
    model: String,
    dimension: usize,
}

impl MockEmbeddingProvider {
    /// Construct a mock that reports `model` and produces `dimension`-long vectors.
    pub fn new(model: impl Into<String>, dimension: usize) -> Self {
        Self {
            model: model.into(),
            dimension,
        }
    }

    fn fixed_vector(&self) -> EmbeddingVector {
        let mut v = Vec::with_capacity(self.dimension);
        for i in 0..self.dimension {
            v.push(((i % 10) + 1) as f32 / 10.0);
        }
        EmbeddingVector(v)
    }
}

#[async_trait]
impl EmbeddingProvider for MockEmbeddingProvider {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbeddingError> {
        Ok(texts.iter().map(|_| self.fixed_vector()).collect())
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn returns_one_vector_per_input() {
        let m = MockEmbeddingProvider::new("test", 8);
        let vs = m.embed(&["a", "b", "c"]).await.unwrap();
        assert_eq!(vs.len(), 3);
        assert!(vs.iter().all(|v| v.dimension() == 8));
    }

    #[tokio::test]
    async fn empty_input_returns_empty_output() {
        let m = MockEmbeddingProvider::new("test", 4);
        let vs = m.embed(&[]).await.unwrap();
        assert!(vs.is_empty());
    }

    #[test]
    fn dimension_and_name_reflect_constructor() {
        let m = MockEmbeddingProvider::new("e5-mock", 384);
        assert_eq!(m.dimension(), 384);
        assert_eq!(m.model_name(), "e5-mock");
    }
}
