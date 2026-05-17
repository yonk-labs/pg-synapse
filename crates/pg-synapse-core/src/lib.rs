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
//! ## Example: one-shot provider call
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
//!
//! ## Example: full Runtime end-to-end
//!
//! The operator-facing API is `Runtime`. Build one with `Runtime::builder()`,
//! register provider factories via plugins, supply agent + profile rows
//! (inline or through a `ProfileSource`), and call `Runtime::execute`.
//!
//! ```
//! use std::sync::Arc;
//!
//! use pg_synapse_core::Runtime;
//! use pg_synapse_core::runtime::test_utils::MockProfileSource;
//! use pg_synapse_core::testing::{MockLlmFactory, MockLlmProvider};
//! use pg_synapse_core::types::{AgentRow, LlmProfileRow};
//!
//! # tokio_test::block_on(async {
//! let mock = Arc::new(MockLlmProvider::new("mock"));
//! mock.push_text("hi");
//!
//! let source = MockProfileSource::new()
//!     .with_llm_profile(LlmProfileRow {
//!         name: "default".into(),
//!         provider: "mock".into(),
//!         model: "mock".into(),
//!         api_key_secret: None,
//!         base_url: None,
//!         params: serde_json::Value::Null,
//!     })
//!     .with_agent(AgentRow {
//!         name: "agent1".into(),
//!         system_prompt: "be brief".into(),
//!         soul: None,
//!         executor_name: "conversation".into(),
//!         llm_profile_main: Some("default".into()),
//!         llm_profile_small: None,
//!         llm_profile_judge: None,
//!         embedding_profile: None,
//!         tools: vec![],
//!         max_iterations: 5,
//!         timeout_ms: 30_000,
//!         cost_cap_usd: None,
//!     });
//!
//! let runtime = Runtime::builder()
//!     .with_plugin(MockLlmFactory::new("mock", mock))
//!     .load_profiles_from(source)
//!     .build()
//!     .await
//!     .unwrap();
//!
//! let outcome = runtime.execute("agent1", "hello").await.unwrap();
//! assert_eq!(outcome.output, "hi");
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
pub mod runtime;
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
pub use llm::{LlmProvider, LlmProviderFactory, ProviderCapabilities};
pub use memory::MemoryProvider;
pub use plugin::{Plugin, Registry};
pub use runtime::{ProfileSource, Runtime, RuntimeBuilder};
pub use tool::{Tool, ToolRegistry};

/// Re-export of `async_trait` so derive macros generated against this crate
/// can name a stable path to the attribute without forcing tool authors to
/// add `async-trait` to their own `Cargo.toml`.
#[doc(hidden)]
pub use async_trait;
