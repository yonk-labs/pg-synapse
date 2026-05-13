//! `pg-synapse-core` is the kernel for the Postgres-native agent-loop runtime.
//!
//! This crate defines:
//!
//! - Six traits: [`Executor`], [`Tool`], [`LlmProvider`], [`EmbeddingProvider`],
//!   [`MemoryProvider`], [`Compressor`].
//! - The data types those traits exchange (see [`types`]).
//! - A complete error taxonomy (see [`error`]).
//! - Scriptable mock providers for testing (see [`testing`]).
//! - The [`Plugin`] trait and [`Registry`] aggregate used by host crates to
//!   wire providers, executors, and tools.
//!
//! No I/O happens in this crate. Hosts (`pg-synapse-pgrx`, `pg-synapse-sidecar`)
//! supply the tokio runtime, database, and network surfaces. Plugin crates
//! supply concrete provider, embedding, memory, and compressor implementations.
//!
//! ## Example
//!
//! ```
//! use pg_synapse_core::testing::MockLlmProvider;
//! use pg_synapse_core::types::CompletionRequest;
//! use pg_synapse_core::LlmProvider;
//!
//! # tokio_test::block_on(async {
//! let mock = MockLlmProvider::new("test-model");
//! mock.push_text("hello, world");
//! let resp = mock.complete(CompletionRequest::default()).await.unwrap();
//! assert_eq!(resp.content.as_deref(), Some("hello, world"));
//! # });
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod compression;
pub mod embedding;
pub mod error;
pub mod executor;
pub mod llm;
pub mod memory;
pub mod plugin;
pub mod testing;
pub mod tool;
pub mod types;

pub use compression::Compressor;
pub use embedding::{EmbeddingProvider, EmbeddingProviderFactory};
pub use error::{
    CompressionError, EmbeddingError, ExecutorError, LlmError, MemoryError, ProviderError,
    RuntimeError, ToolError,
};
pub use executor::Executor;
pub use llm::{LlmProvider, LlmProviderFactory};
pub use memory::MemoryProvider;
pub use plugin::{Plugin, Registry};
pub use tool::{Tool, ToolRegistry};
