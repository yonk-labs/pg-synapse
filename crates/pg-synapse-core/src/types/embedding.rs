//! Embedding vector newtype.

use serde::{Deserialize, Serialize};

/// A dense embedding vector returned by an [`crate::EmbeddingProvider`].
///
/// Newtype over `Vec<f32>` so the kernel can attach helpers (dimension check,
/// slicing) and so call sites get a more meaningful type than a bare `Vec`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingVector(
    /// Dense float values, length equal to the embedding model's dimension.
    pub Vec<f32>,
);

impl EmbeddingVector {
    /// Wrap an existing `Vec<f32>`.
    pub fn new(v: Vec<f32>) -> Self {
        Self(v)
    }

    /// Dimension (number of floats).
    pub fn dimension(&self) -> usize {
        self.0.len()
    }

    /// Borrow as a slice.
    pub fn as_slice(&self) -> &[f32] {
        &self.0
    }

    /// Consume self and return the inner `Vec`.
    pub fn into_inner(self) -> Vec<f32> {
        self.0
    }
}

impl From<Vec<f32>> for EmbeddingVector {
    fn from(v: Vec<f32>) -> Self {
        Self(v)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimension_matches_length() {
        let v = EmbeddingVector(vec![0.1, 0.2, 0.3]);
        assert_eq!(v.dimension(), 3);
        assert_eq!(v.as_slice(), &[0.1, 0.2, 0.3]);
    }

    #[test]
    fn roundtrips_json() {
        let v = EmbeddingVector(vec![1.0, 2.0, 3.0]);
        let s = serde_json::to_string(&v).unwrap();
        let back: EmbeddingVector = serde_json::from_str(&s).unwrap();
        assert_eq!(v, back);
    }

    #[test]
    fn into_inner_returns_vec() {
        let v = EmbeddingVector(vec![1.0]);
        let inner = v.into_inner();
        assert_eq!(inner, vec![1.0]);
    }

    #[test]
    fn from_vec_works() {
        let v: EmbeddingVector = vec![0.5, 1.5].into();
        assert_eq!(v.dimension(), 2);
    }

    #[test]
    fn debug_format_shows_values() {
        let v = EmbeddingVector(vec![1.0]);
        let s = format!("{:?}", v);
        assert!(s.contains("1.0"));
    }
}
