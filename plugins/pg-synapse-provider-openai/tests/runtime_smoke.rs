//! Smoke test: the plugin registers cleanly into a [`Runtime`] via
//! [`Runtime::builder().with_plugin(OpenAiProviderFactory::default())`].
//!
//! We don't run an execution here (that would need a live endpoint or another
//! mock); we just confirm the registry side of the plugin contract.

use pg_synapse_core::Runtime;
use pg_synapse_core::runtime::test_utils::MockProfileSource;
use pg_synapse_core::types::{AgentRow, LlmProfileRow};
use pg_synapse_provider_openai::OpenAiProviderFactory;

#[tokio::test]
async fn runtime_builder_accepts_openai_plugin() {
    let source = MockProfileSource::new()
        .with_llm_profile(LlmProfileRow {
            name: "default".into(),
            provider: "openai".into(),
            model: "gpt-test".into(),
            api_key_secret: None,
            base_url: Some("http://127.0.0.1:0/v1".into()),
            params: serde_json::Value::Null,
        })
        .with_agent(AgentRow {
            name: "agent1".into(),
            system_prompt: "Be brief.".into(),
            soul: None,
            executor_name: "conversation".into(),
            llm_profile_main: Some("default".into()),
            llm_profile_small: None,
            llm_profile_judge: None,
            embedding_profile: None,
            tools: vec![],
            max_iterations: 4,
            timeout_ms: 30_000,
            cost_cap_usd: None,
            trace_level: None,
        });

    let runtime = Runtime::builder()
        .with_plugin(OpenAiProviderFactory)
        .load_profiles_from(source)
        .build()
        .await
        .expect("runtime builds with openai plugin");

    // Sanity: the runtime exists. We avoid calling execute() here because that
    // would dial the (intentionally unreachable) endpoint.
    let _ = runtime;
}
