//! The [`Compressor`] trait. No default implementation ships in v0.1;
//! `lede-pg-synapse` and other plugins fill this slot.

use async_trait::async_trait;

use crate::error::CompressionError;
use crate::types::{Compressed, CompressionBudget, Message};

/// Squeeze a slice of messages into a [`Compressed`] summary that fits the
/// supplied [`CompressionBudget`].
///
/// Implementations must be `Send + Sync`.
///
/// ## Example
///
/// ```
/// use async_trait::async_trait;
/// use pg_synapse_core::{CompressionError, Compressor};
/// use pg_synapse_core::types::{Compressed, CompressionBudget, Message};
///
/// struct ConstantCompressor;
///
/// #[async_trait]
/// impl Compressor for ConstantCompressor {
///     async fn compress(
///         &self,
///         _messages: &[Message],
///         _budget: CompressionBudget,
///     ) -> Result<Compressed, CompressionError> {
///         Ok(Compressed {
///             summary: "(compressed)".into(),
///             collapsed_indices: vec![],
///         })
///     }
/// }
/// ```
#[async_trait]
pub trait Compressor: Send + Sync {
    /// Compress `messages` into a single summary within `budget`.
    async fn compress(
        &self,
        messages: &[Message],
        budget: CompressionBudget,
    ) -> Result<Compressed, CompressionError>;
}
