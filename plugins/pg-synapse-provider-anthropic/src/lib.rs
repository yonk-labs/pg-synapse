//! pg_synapse provider plugin for Anthropic Claude models.
//!
//! Implements the Anthropic Messages API (`POST /v1/messages`) as documented at
//! <https://docs.anthropic.com/en/api/messages>.
//!
//! v0.1 scope:
//!
//! * Sync `complete()` against `POST /v1/messages`.
//! * System messages extracted from the message list into the top-level
//!   `"system"` field.
//! * Tool calls in both directions: request `tools` (using `input_schema`) and
//!   response `tool_use` content blocks.
//! * Tool results returned as user-role messages with `"tool_result"` blocks.
//! * Prompt caching: when `params.prompt_caching == true`, the system block
//!   gets `cache_control: {type: ephemeral}`.
//! * Streaming is intentionally **not** implemented (kernel decision D8).
//!
//! ## Example
//!
//! ```ignore
//! use pg_synapse_core::Runtime;
//! use pg_synapse_core::runtime::test_utils::MockProfileSource;
//! use pg_synapse_core::types::{AgentRow, LlmProfileRow};
//! use pg_synapse_provider_anthropic::AnthropicProviderFactory;
//!
//! let source = MockProfileSource::new()
//!     .with_llm_profile(LlmProfileRow {
//!         name: "claude".into(),
//!         provider: "anthropic".into(),
//!         model: "claude-3-5-haiku-20241022".into(),
//!         api_key_secret: Some("anthropic_key".into()),
//!         base_url: None,
//!         params: serde_json::json!({ "prompt_caching": true }),
//!     })
//!     .with_agent(AgentRow {
//!         name: "assistant".into(),
//!         system_prompt: "Be concise.".into(),
//!         soul: None,
//!         executor_name: "conversation".into(),
//!         llm_profile_main: Some("claude".into()),
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
//!     .with_plugin(AnthropicProviderFactory::default())
//!     .load_profiles_from(source)
//!     .build()
//!     .await
//!     .unwrap();
//! ```

#![forbid(unsafe_code)]
#![warn(missing_docs)]

pub mod client;
pub mod factory;

pub use client::AnthropicProvider;
pub use factory::AnthropicProviderFactory;
