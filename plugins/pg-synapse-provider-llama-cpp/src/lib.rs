//! pg_synapse provider plugin for the llama.cpp server (OpenAI-compatible).
//!
//! The `llama-server` binary (from <https://github.com/ggml-org/llama.cpp>)
//! exposes an OpenAI-compatible REST API. This crate wires it into the
//! pg_synapse provider system as provider name `"llama-cpp"`.
//!
//! Default endpoint: `http://127.0.0.1:8080/v1`.
//!
//! ## Surfaces
//!
//! * [`LlamaCppProvider`] (implements [`LlmProvider`]) for chat completions via
//!   `POST {base_url}/chat/completions`.
//! * [`LlamaCppEmbeddingProvider`] (implements [`EmbeddingProvider`]) for
//!   embedding vectors via `POST {base_url}/embeddings`.
//! * [`LlamaCppProviderFactory`] + [`LlamaCppEmbeddingFactory`] for building
//!   providers from profile rows at runtime.
//!
//! ## Optional: GGUF download
//!
//! Enable the `download` feature to gain access to
//! [`download::download_gguf`] and [`download::granite_micro`]:
//!
//! ```toml
//! [dependencies]
//! pg-synapse-provider-llama-cpp = { version = "0.1", features = ["download"] }
//! ```
//!
//! The download helper fetches a GGUF from Hugging Face into
//! `~/.cache/pg-synapse/models/{repo}/{file}` and skips the download if the
//! file is already present.
//!
//! ## Granite micro coordinates
//!
//! The bundled `granite_micro()` helper returns:
//! `("lmstudio-community/granite-3.0-2b-instruct-GGUF", "granite-3.0-2b-instruct-Q4_K_M.gguf", "main")`
//!
//! Fallback (if the primary is unavailable):
//! `("bartowski/granite-3.1-2b-instruct-GGUF", "granite-3.1-2b-instruct-Q4_K_M.gguf", "main")`
//!
//! ## Example
//!
//! ```ignore
//! use pg_synapse_core::Runtime;
//! use pg_synapse_core::types::LlmProfileRow;
//! use pg_synapse_provider_llama_cpp::LlamaCppProviderFactory;
//!
//! let factory = LlamaCppProviderFactory::default();
//! // Register via Runtime builder:
//! // Runtime::builder().with_plugin(factory) ...
//! let _ = factory;
//! ```
//!
//! [`LlmProvider`]: pg_synapse_core::LlmProvider
//! [`EmbeddingProvider`]: pg_synapse_core::EmbeddingProvider

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod factory;

#[cfg(feature = "download")]
pub mod download;

pub use client::{LlamaCppEmbeddingProvider, LlamaCppProvider};
pub use factory::{LlamaCppEmbeddingFactory, LlamaCppProviderFactory};
