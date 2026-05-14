//! Scriptable mock providers and tools for unit and integration tests.
//!
//! These types live in the public surface of `pg-synapse-core` so plugin
//! authors and host crates can drive realistic kernel paths without standing
//! up real backends.

pub mod mock_embedding;
pub mod mock_factories;
pub mod mock_llm;
pub mod mock_tool;

pub use mock_embedding::MockEmbeddingProvider;
pub use mock_factories::{MockEmbeddingFactory, MockLlmFactory};
pub use mock_llm::{MockLlmProvider, MockResponse};
pub use mock_tool::MockTool;
