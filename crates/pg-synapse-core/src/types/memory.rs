//! Memory-provider data types.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Identifier for one entry in a memory backend.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
pub struct MemoryId(
    /// Backend-issued UUID for the entry.
    pub Uuid,
);

impl MemoryId {
    /// Construct from a UUID.
    pub fn new(id: Uuid) -> Self {
        Self(id)
    }

    /// Unwrap into a UUID.
    pub fn into_uuid(self) -> Uuid {
        self.0
    }
}

/// Filter / namespace for a memory read or write.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryScope {
    /// Agent name. Required by most backends.
    pub agent_name: String,
    /// Optional per-session bucket.
    pub session_id: Option<String>,
    /// Free-form tags backends may use for further filtering.
    pub tags: Vec<String>,
}

/// One entry returned by or written into a memory backend.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemoryEntry {
    /// Backend-issued identifier.
    pub id: MemoryId,
    /// Textual content of the entry.
    pub content: String,
    /// Where the content originated (e.g. "user", "tool:weather", "executor").
    pub source: String,
    /// When the entry was recorded.
    pub created_at: DateTime<Utc>,
    /// Tags attached at write time.
    pub tags: Vec<String>,
}

/// A point-in-time read of a memory scope. Order is backend-defined.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct MemorySnapshot(
    /// Entries in backend-defined order.
    pub Vec<MemoryEntry>,
);

impl MemorySnapshot {
    /// Wrap an existing `Vec<MemoryEntry>`.
    pub fn new(entries: Vec<MemoryEntry>) -> Self {
        Self(entries)
    }

    /// Number of entries.
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Is the snapshot empty?
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Borrow the entries as a slice.
    pub fn as_slice(&self) -> &[MemoryEntry] {
        &self.0
    }

    /// Consume self and return the entries.
    pub fn into_inner(self) -> Vec<MemoryEntry> {
        self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_entry() -> MemoryEntry {
        MemoryEntry {
            id: MemoryId(Uuid::nil()),
            content: "the user likes octopuses".into(),
            source: "user".into(),
            created_at: DateTime::from_timestamp(0, 0).unwrap(),
            tags: vec!["fact".into()],
        }
    }

    #[test]
    fn memory_entry_roundtrips() {
        let e = sample_entry();
        let s = serde_json::to_string(&e).unwrap();
        let back: MemoryEntry = serde_json::from_str(&s).unwrap();
        assert_eq!(e, back);
    }

    #[test]
    fn memory_scope_default_empty() {
        let s = MemoryScope::default();
        assert!(s.agent_name.is_empty());
        assert!(s.session_id.is_none());
        assert!(s.tags.is_empty());
    }

    #[test]
    fn memory_snapshot_len_matches() {
        let snap = MemorySnapshot::new(vec![sample_entry(), sample_entry()]);
        assert_eq!(snap.len(), 2);
        assert!(!snap.is_empty());
    }

    #[test]
    fn memory_id_uuid_roundtrip() {
        let id = MemoryId::new(Uuid::nil());
        assert_eq!(id.into_uuid(), Uuid::nil());
    }

    #[test]
    fn memory_entry_debug_includes_source() {
        let e = sample_entry();
        let s = format!("{:?}", e);
        assert!(s.contains("user"));
    }
}
