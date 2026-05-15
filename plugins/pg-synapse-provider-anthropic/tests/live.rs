#![cfg(feature = "live-tests")]

//! Live tests against the real Anthropic API.
//!
//! Run with:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-ant-... \
//! cargo test -p pg-synapse-provider-anthropic --features live-tests --test live
//! ```
//!
//! Optionally override the model:
//!
//! ```text
//! ANTHROPIC_API_KEY=sk-ant-... \
//! PG_SYNAPSE_TEST_ANTHROPIC_MODEL=claude-3-5-sonnet-20241022 \
//! cargo test -p pg-synapse-provider-anthropic --features live-tests --test live
//! ```
//!
//! If `ANTHROPIC_API_KEY` is unset the tests skip with a printed message
//! rather than panicking.

use chrono::Utc;
use uuid::Uuid;

use pg_synapse_core::LlmProvider;
use pg_synapse_core::types::{CompletionRequest, Message, Role};
use pg_synapse_provider_anthropic::AnthropicProvider;

fn api_key_and_model() -> Option<(String, String)> {
    let key = std::env::var("ANTHROPIC_API_KEY").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_ANTHROPIC_MODEL")
        .unwrap_or_else(|_| "claude-3-5-haiku-20241022".to_string());
    Some((key, model))
}

#[tokio::test]
async fn live_completes_against_anthropic_api() {
    let Some((key, model)) = api_key_and_model() else {
        eprintln!("SKIP: ANTHROPIC_API_KEY not set");
        return;
    };

    let p = AnthropicProvider::new(&model, "https://api.anthropic.com").with_api_key(key);
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
        .expect("live Anthropic complete failed");

    let txt = resp.content.unwrap_or_default();
    eprintln!("LIVE Anthropic responded: {txt:?}");
    assert!(!txt.is_empty(), "empty response from Anthropic API");
    assert!(resp.usage.tokens_in > 0, "expected non-zero input tokens");
    assert!(resp.usage.tokens_out > 0, "expected non-zero output tokens");
}
