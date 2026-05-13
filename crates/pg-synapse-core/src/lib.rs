//! `pg-synapse-core` is the kernel for the Postgres-native agent-loop runtime.
//!
//! This crate defines:
//!
//! - Six traits: [`Executor`], [`Tool`], [`LlmProvider`], [`EmbeddingProvider`],
//!   [`MemoryProvider`], [`Compressor`].
//! - The data types those traits exchange (see [`types`]).
//! - A complete error taxonomy (see [`error`]).
//! - Scriptable mock providers for testing (see [`testing`]).
//! - The `Plugin` and `Registry` shape used by host crates to wire providers.
//!
//! No I/O happens in this crate. Hosts (`pg-synapse-pgrx`, `pg-synapse-sidecar`)
//! supply the tokio runtime, database, and network surfaces. Plugin crates
//! supply concrete provider, embedding, memory, and compressor implementations.

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
