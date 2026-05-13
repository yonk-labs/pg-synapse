//! Compression data types.

use serde::{Deserialize, Serialize};

/// The result of a compression pass over a slice of messages.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Compressed {
    /// Natural-language summary that replaces the collapsed messages.
    pub summary: String,
    /// Indices into the input slice of messages that were collapsed into the summary.
    pub collapsed_indices: Vec<u32>,
}

/// Budget supplied to a [`crate::Compressor`].
///
/// `target_tokens` is the soft goal; `hard_limit_tokens` is the cap above
/// which the executor will refuse to continue without further compression.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct CompressionBudget {
    /// Desired post-compression token count.
    pub target_tokens: u32,
    /// Maximum acceptable post-compression token count.
    pub hard_limit_tokens: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compressed_roundtrips() {
        let c = Compressed {
            summary: "talked about octopuses".into(),
            collapsed_indices: vec![0, 1, 2],
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: Compressed = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn compression_budget_roundtrips() {
        let b = CompressionBudget {
            target_tokens: 1000,
            hard_limit_tokens: 2000,
        };
        let s = serde_json::to_string(&b).unwrap();
        let back: CompressionBudget = serde_json::from_str(&s).unwrap();
        assert_eq!(b, back);
    }

    #[test]
    fn defaults_are_zero() {
        let b = CompressionBudget::default();
        assert_eq!(b.target_tokens, 0);
        assert_eq!(b.hard_limit_tokens, 0);
    }

    #[test]
    fn compressed_debug_shows_summary() {
        let c = Compressed {
            summary: "hello".into(),
            collapsed_indices: vec![],
        };
        let s = format!("{:?}", c);
        assert!(s.contains("hello"));
    }
}
