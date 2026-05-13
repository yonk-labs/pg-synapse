//! The [`MemoryProvider`] trait. No default implementation ships in v0.1;
//! `stele-pg-synapse` and other plugins fill this slot.

use async_trait::async_trait;

use crate::error::MemoryError;
use crate::types::{MemoryEntry, MemoryId, MemoryScope, MemorySnapshot};

/// Read / write / search an external memory backend.
///
/// Implementations are typically held as `Arc<dyn MemoryProvider>` inside
/// [`crate::types::ExecutionContext`].
///
/// ## Example
///
/// ```
/// use async_trait::async_trait;
/// use pg_synapse_core::{MemoryError, MemoryProvider};
/// use pg_synapse_core::types::{MemoryEntry, MemoryId, MemoryScope, MemorySnapshot};
/// use uuid::Uuid;
///
/// struct Stub;
///
/// #[async_trait]
/// impl MemoryProvider for Stub {
///     async fn read(&self, _scope: MemoryScope)
///         -> Result<MemorySnapshot, MemoryError> {
///         Ok(MemorySnapshot::default())
///     }
///     async fn write(&self, _scope: MemoryScope, _entry: MemoryEntry)
///         -> Result<MemoryId, MemoryError> {
///         Ok(MemoryId(Uuid::nil()))
///     }
///     async fn search(&self, _scope: MemoryScope, _query: &str, _limit: usize)
///         -> Result<Vec<MemoryEntry>, MemoryError> {
///         Ok(vec![])
///     }
/// }
/// ```
#[async_trait]
pub trait MemoryProvider: Send + Sync {
    /// Read all entries for a given scope (subject to backend pagination).
    async fn read(&self, scope: MemoryScope) -> Result<MemorySnapshot, MemoryError>;

    /// Write one entry under a scope. Returns the backend-issued ID.
    async fn write(&self, scope: MemoryScope, entry: MemoryEntry) -> Result<MemoryId, MemoryError>;

    /// Search a scope for entries relevant to `query`, returning up to `limit`.
    async fn search(
        &self,
        scope: MemoryScope,
        query: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, MemoryError>;
}
