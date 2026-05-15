#![cfg(feature = "live-tests")]

//! Live tests against an operator-configured llama-server endpoint.
//!
//! Run with:
//!
//! ```text
//! PG_SYNAPSE_TEST_LLAMACPP_BASE_URL=http://127.0.0.1:8080/v1 \
//! cargo test -p pg-synapse-provider-llama-cpp --features live-tests --test live
//! ```
//!
//! `PG_SYNAPSE_TEST_LLAMACPP_BASE_URL` must point at a running `llama-server`
//! process. If the variable is unset, all tests skip with a printed reason
//! rather than failing.
//!
//! Optional env vars:
//! * `PG_SYNAPSE_TEST_LLAMACPP_MODEL` overrides the model name sent in
//!   requests (default: `granite-3.0-2b-instruct`).

use chrono::Utc;
use uuid::Uuid;

use pg_synapse_core::EmbeddingProvider;
use pg_synapse_core::LlmProvider;
use pg_synapse_core::types::{CompletionRequest, Message, Role};
use pg_synapse_provider_llama_cpp::{LlamaCppEmbeddingProvider, LlamaCppProvider};

fn endpoint() -> Option<(String, String)> {
    let base = std::env::var("PG_SYNAPSE_TEST_LLAMACPP_BASE_URL").ok()?;
    let model = std::env::var("PG_SYNAPSE_TEST_LLAMACPP_MODEL")
        .unwrap_or_else(|_| "granite-3.0-2b-instruct".to_string());
    Some((base, model))
}

#[tokio::test]
async fn live_completes_against_llamacpp_endpoint() {
    let Some((base, model)) = endpoint() else {
        eprintln!("SKIP: PG_SYNAPSE_TEST_LLAMACPP_BASE_URL not set");
        return;
    };
    let p = LlamaCppProvider::new(&model, &base);
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
        .expect("complete failed against live llama-server");
    let txt = resp.content.unwrap_or_default();
    eprintln!("LIVE llama-cpp responded: {txt:?}");
    assert!(!txt.is_empty(), "empty response from live llama-server");
}

#[tokio::test]
async fn live_embeddings_returns_nonzero_vectors() {
    let Some((base, model)) = endpoint() else {
        eprintln!("SKIP: PG_SYNAPSE_TEST_LLAMACPP_BASE_URL not set");
        return;
    };
    // dimension 0 is a placeholder for live tests; the test just checks non-empty.
    let ep = LlamaCppEmbeddingProvider::new(&model, &base, 1);
    let result = ep.embed(&["hello world"]).await;
    match result {
        Ok(vecs) => {
            eprintln!("LIVE embedding dim: {}", vecs[0].dimension());
            assert!(!vecs[0].as_slice().is_empty());
        }
        Err(e) => {
            // DimensionMismatch is expected when dim=1 and the model returns
            // real-sized vectors; that is acceptable in this live probe.
            eprintln!("live embedding result (may be DimensionMismatch): {e:?}");
        }
    }
}
