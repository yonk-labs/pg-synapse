//! pg_synapse provider plugin for OpenAI and OpenAI-compatible endpoints.
//!
//! Targets the OpenAI Chat Completions wire shape, which a number of
//! self-hosted runtimes also expose: vLLM, llama-cpp-server, LM Studio, and the
//! Ollama OpenAI shim. Configure the endpoint via [`LlmProfileRow::base_url`]
//! or `params.base_url`. For vLLM on `http://host:port/v1`, set
//! `base_url = "http://host:port/v1"`.
//!
//! v0.1 scope:
//!
//! * Sync `complete()` against `POST /chat/completions`.
//! * Tool calls in both directions (request `tools` and response `tool_calls`).
//! * Usage accounting with optional USD cost.
//! * Model discovery via `GET /models`.
//! * Streaming is intentionally **not** implemented (kernel decision D8).
//!
//! ## Example
//!
//! ```ignore
//! use pg_synapse_core::Runtime;
//! use pg_synapse_core::runtime::test_utils::MockProfileSource;
//! use pg_synapse_core::types::{AgentRow, LlmProfileRow};
//! use pg_synapse_provider_openai::OpenAiProviderFactory;
//!
//! let source = MockProfileSource::new()
//!     .with_llm_profile(LlmProfileRow {
//!         name: "vllm".into(),
//!         provider: "openai".into(),
//!         model: "granite-3.1-2b-instruct".into(),
//!         api_key_secret: None,
//!         base_url: Some("http://192.168.1.193:8000/v1".into()),
//!         params: serde_json::json!({}),
//!     })
//!     .with_agent(AgentRow {
//!         name: "demo".into(),
//!         system_prompt: "Be brief.".into(),
//!         soul: None,
//!         executor_name: "conversation".into(),
//!         llm_profile_main: Some("vllm".into()),
//!         llm_profile_small: None,
//!         llm_profile_judge: None,
//!         embedding_profile: None,
//!         tools: vec![],
//!         max_iterations: 4,
//!         timeout_ms: 30_000,
//!         cost_cap_usd: None,
//!         trace_level: None,
//!     });
//!
//! let _runtime = Runtime::builder()
//!     .with_plugin(OpenAiProviderFactory::default())
//!     .load_profiles_from(source)
//!     .build()
//!     .await
//!     .unwrap();
//! ```
//!
//! [`LlmProfileRow::base_url`]: pg_synapse_core::types::LlmProfileRow::base_url

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod factory;

pub use client::OpenAiProvider;
pub use factory::OpenAiProviderFactory;
