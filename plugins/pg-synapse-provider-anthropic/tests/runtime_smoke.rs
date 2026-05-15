//! Smoke test: the plugin registers cleanly into a [`Runtime`] via
//! `Runtime::builder().with_plugin(AnthropicProviderFactory::default())`.
//!
//! No execution is triggered; this only verifies that the plugin contract
//! (registry registration, factory naming, etc.) is satisfied.

use pg_synapse_core::Runtime;
use pg_synapse_core::runtime::test_utils::MockProfileSource;
use pg_synapse_core::types::{AgentRow, LlmProfileRow};
use pg_synapse_provider_anthropic::AnthropicProviderFactory;

#[tokio::test]
async fn runtime_builder_accepts_anthropic_plugin() {
    let source = MockProfileSource::new()
        .with_llm_profile(LlmProfileRow {
            name: "claude".into(),
            provider: "anthropic".into(),
            model: "claude-3-5-haiku-20241022".into(),
            api_key_secret: None,
            base_url: Some("https://api.anthropic.com".into()),
            params: serde_json::Value::Null,
        })
        .with_agent(AgentRow {
            name: "agent1".into(),
            system_prompt: "Be brief.".into(),
            soul: None,
            executor_name: "conversation".into(),
            llm_profile_main: Some("claude".into()),
            llm_profile_small: None,
            llm_profile_judge: None,
            embedding_profile: None,
            tools: vec![],
            max_iterations: 4,
            timeout_ms: 30_000,
            cost_cap_usd: None,
        });

    let runtime = Runtime::builder()
        .with_plugin(AnthropicProviderFactory)
        .load_profiles_from(source)
        .build()
        .await
        .expect("runtime builds with anthropic plugin");

    // Verify the runtime was created. We avoid calling execute() because
    // that would dial the real Anthropic endpoint.
    let _ = runtime;
}
