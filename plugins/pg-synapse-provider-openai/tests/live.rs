#![cfg(feature = "live-tests")]

//! Live tests against an operator-configured OpenAI-compatible endpoint.
//!
//! Run with:
//!
//! ```text
//! PG_SYNAPSE_TEST_LLM_BASE_URL=http://192.168.1.193:8000/v1 \
//! PG_SYNAPSE_TEST_LLM_MODEL=granite-3.1-2b-instruct \
//! cargo test -p pg-synapse-provider-openai --features live-tests --test live
//! ```
//!
//! The endpoint must be reachable from the test host. If
//! `PG_SYNAPSE_TEST_LLM_BASE_URL` is unset, the tests skip themselves with a
//! printed reason rather than panicking.

use chrono::Utc;
use uuid::Uuid;

use pg_synapse_core::LlmProvider;
use pg_synapse_core::Runtime;
use pg_synapse_core::runtime::test_utils::MockProfileSource;
use pg_synapse_core::types::{
    AgentRow, CompletionRequest, LlmProfileRow, Message, OutcomeStatus, Role,
};
use pg_synapse_provider_openai::{OpenAiProvider, OpenAiProviderFactory};

fn endpoint() -> Option<(String, String)> {
    let base = std::env::var("PG_SYNAPSE_TEST_LLM_BASE_URL").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_LLM_MODEL")
        .unwrap_or_else(|_| "granite-3.1-2b-instruct".to_string());
    Some((base, model))
}

#[tokio::test]
async fn live_completes_against_configured_endpoint() {
    let Some((base, model)) = endpoint() else {
        eprintln!("SKIP: PG_SYNAPSE_TEST_LLM_BASE_URL not set");
        return;
    };
    let p = OpenAiProvider::new(&model, &base);
    let req = CompletionRequest {
        messages: vec![Message {
            execution_id: Uuid::nil(),
            seq: 0,
            role: Role::User,
            content: Some("Reply with only the word 'pong'.".into()),
            tool_call_id: None,
            tool_name: None,
            tool_input: None,
            tool_output: None,
            timestamp: Utc::now(),
        }],
        tools: vec![],
        model: Some(model.clone()),
        temperature: Some(0.0),
        max_tokens: Some(50),
        params: serde_json::Value::Null,
    };
    let resp = p
        .complete(req)
        .await
        .expect("complete failed against live endpoint");
    let txt = resp.content.unwrap_or_default();
    eprintln!("LIVE LLM responded: {txt:?}");
    assert!(!txt.is_empty(), "empty response from live LLM");
}

/// Full agent loop through the kernel `Runtime` against the live endpoint.
/// Exercises `execute_inner` (including the wall-clock timeout wrapper) and the
/// conversation executor end to end, not just the provider in isolation.
#[tokio::test]
async fn live_agent_run_completes_within_budget() {
    let Some((base, model)) = endpoint() else {
        eprintln!("SKIP: PG_SYNAPSE_TEST_LLM_BASE_URL not set");
        return;
    };
    let source = MockProfileSource::new()
        .with_llm_profile(LlmProfileRow {
            name: "default".into(),
            provider: "openai".into(),
            model: model.clone(),
            api_key_secret: None,
            base_url: Some(base.clone()),
            params: serde_json::Value::Null,
        })
        .with_agent(AgentRow {
            name: "greeter".into(),
            system_prompt: "You are terse. Answer in a single word.".into(),
            soul: None,
            executor_name: "conversation".into(),
            llm_profile_main: Some("default".into()),
            llm_profile_small: None,
            llm_profile_judge: None,
            embedding_profile: None,
            tools: vec![],
            max_iterations: 4,
            timeout_ms: 60_000,
            cost_cap_usd: None,
            trace_level: None,
        });

    let runtime = Runtime::builder()
        .with_plugin(OpenAiProviderFactory)
        .load_profiles_from(source)
        .build()
        .await
        .expect("runtime builds with openai plugin");

    let outcome = runtime
        .execute("greeter", "Reply with a one-word greeting.")
        .await
        .expect("live agent run failed");
    eprintln!(
        "LIVE agent outcome: status={:?} output={:?}",
        outcome.status, outcome.output
    );
    assert_eq!(outcome.status, OutcomeStatus::Completed);
    assert!(!outcome.output.is_empty(), "agent produced no output");
}

#[tokio::test]
async fn live_lists_models() {
    let Some((base, _)) = endpoint() else {
        eprintln!("SKIP: PG_SYNAPSE_TEST_LLM_BASE_URL not set");
        return;
    };
    let p = OpenAiProvider::new("ignored", &base);
    let models = p
        .list_models()
        .await
        .expect("list_models failed against live endpoint");
    eprintln!("LIVE models: {models:?}");
    assert!(!models.is_empty(), "live endpoint returned no models");
}
