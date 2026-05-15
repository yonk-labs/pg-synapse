//! Integration tests for the llama-cpp provider backed by [wiremock].
//!
//! All tests are hermetic: no real llama-server is needed. Each test spins up
//! a local mock server that mimics the OpenAI-compatible endpoints that
//! `llama-server` exposes.

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pg_synapse_core::EmbeddingProvider;
use pg_synapse_core::LlmProvider;
use pg_synapse_core::LlmProviderFactory;
use pg_synapse_core::error::LlmError;
use pg_synapse_core::types::{
    CompletionRequest, EmbeddingProfileRow, LlmProfileRow, Message, Role, ToolDefinition,
    ToolSchema,
};
use pg_synapse_provider_llama_cpp::{
    LlamaCppEmbeddingFactory, LlamaCppEmbeddingProvider, LlamaCppProvider, LlamaCppProviderFactory,
};

fn user_msg(text: &str) -> Message {
    Message {
        execution_id: Uuid::nil(),
        seq: 0,
        role: Role::User,
        content: Some(text.into()),
        tool_call_id: None,
        tool_name: None,
        tool_input: None,
        tool_output: None,
        timestamp: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// 1. Chat completion: happy path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_returns_text_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "Pong!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 10, "completion_tokens": 3, "total_tokens": 13 }
        })))
        .mount(&server)
        .await;

    let p = LlamaCppProvider::new("granite-3.0-2b-instruct", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("ping")],
        tools: vec![],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("Pong!"));
    assert_eq!(resp.finish_reason, "stop");
    assert_eq!(resp.usage.tokens_in, 10);
    assert_eq!(resp.usage.tokens_out, 3);
    assert!(resp.tool_calls.is_empty());
    // llama-cpp always returns cost_usd = None (local, no cost accounting).
    assert_eq!(resp.usage.cost_usd, None);
}

// ---------------------------------------------------------------------------
// 2. Chat completion: tool_calls response
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_returns_tool_calls() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": {
                    "role": "assistant",
                    "content": null,
                    "tool_calls": [{
                        "id": "call_abc",
                        "type": "function",
                        "function": {
                            "name": "sql_query",
                            "arguments": "{\"query\":\"SELECT 1\",\"params\":[]}"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 20, "completion_tokens": 10 }
        })))
        .mount(&server)
        .await;

    let p = LlamaCppProvider::new("granite-3.0-2b-instruct", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("run a query")],
        tools: vec![ToolDefinition {
            name: "sql_query".into(),
            description: "Execute a SQL SELECT".into(),
            schema: ToolSchema::default(),
        }],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_abc");
    assert_eq!(resp.tool_calls[0].name, "sql_query");
    assert_eq!(resp.tool_calls[0].args["query"], "SELECT 1");
    assert_eq!(resp.finish_reason, "tool_calls");
}

// ---------------------------------------------------------------------------
// 3. 4xx error mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_maps_401_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;

    let p = LlamaCppProvider::new("m", server.uri()).with_api_key("bad-key");
    let err = p
        .complete(CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .unwrap_err();
    assert!(
        matches!(err, LlmError::Auth(ref p) if p == "llama-cpp"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn complete_maps_500_to_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("internal error"))
        .mount(&server)
        .await;

    let p = LlamaCppProvider::new("m", server.uri());
    let err = p
        .complete(CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .unwrap_err();
    match err {
        LlmError::Provider { provider, reason } => {
            assert_eq!(provider, "llama-cpp");
            assert!(reason.contains("500"), "reason: {reason}");
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 4. Embeddings: returns vectors of the right dimension
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embeddings_returns_vectors_of_right_dimension() {
    let server = MockServer::start().await;
    let dim: usize = 4;
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "embedding": [0.1, 0.2, 0.3, 0.4], "index": 0 },
                { "embedding": [0.5, 0.6, 0.7, 0.8], "index": 1 }
            ]
        })))
        .mount(&server)
        .await;

    let ep = LlamaCppEmbeddingProvider::new("nomic-embed-text", server.uri(), dim);
    let vecs = ep.embed(&["hello", "world"]).await.expect("embed ok");
    assert_eq!(vecs.len(), 2);
    assert_eq!(vecs[0].dimension(), dim);
    assert_eq!(vecs[1].dimension(), dim);
    assert!((vecs[0].as_slice()[0] - 0.1_f32).abs() < 1e-5);
    assert!((vecs[1].as_slice()[3] - 0.8_f32).abs() < 1e-5);
}

// ---------------------------------------------------------------------------
// 5. Factory builds provider with base_url from profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn factory_builds_provider_with_base_url_from_profile() {
    let f = LlamaCppProviderFactory;
    let profile = LlmProfileRow {
        name: "local-granite".into(),
        provider: "llama-cpp".into(),
        model: "granite-3.0-2b-instruct".into(),
        api_key_secret: None,
        base_url: Some("http://127.0.0.1:8080/v1".into()),
        params: serde_json::json!({}),
    };
    let p = f.build(profile).expect("factory build ok");
    assert_eq!(p.model_name(), "granite-3.0-2b-instruct");
}

// ---------------------------------------------------------------------------
// 6. Embedding factory builds provider with dimension from profile
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embedding_factory_builds_with_dimension() {
    use pg_synapse_core::EmbeddingProviderFactory;

    let f = LlamaCppEmbeddingFactory;
    let profile = EmbeddingProfileRow {
        name: "local-embed".into(),
        provider: "llama-cpp".into(),
        model: "nomic-embed-text".into(),
        dimension: 768,
        api_key_secret: None,
        base_url: Some("http://127.0.0.1:8080/v1".into()),
        params: serde_json::json!({}),
    };
    let p = f.build(profile).expect("embedding factory build ok");
    assert_eq!(p.dimension(), 768);
    assert_eq!(p.model_name(), "nomic-embed-text");
}

// ---------------------------------------------------------------------------
// 7. stream() returns the expected v0.1 unimplemented error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_returns_unimplemented_error_in_v01() {
    let p = LlamaCppProvider::new("m", "http://example.invalid");
    let err = p
        .stream(CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .err()
        .expect("stream errors in v0.1");
    match err {
        LlmError::Provider { reason, .. } => {
            assert!(
                reason.contains("streaming not implemented"),
                "unexpected reason: {reason}"
            );
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// 8. Embedding dimension mismatch is surfaced as DimensionMismatch error
// ---------------------------------------------------------------------------

#[tokio::test]
async fn embeddings_dimension_mismatch_returns_error() {
    use pg_synapse_core::error::EmbeddingError;

    let server = MockServer::start().await;
    // Server returns 3-float vectors but provider expects 4.
    Mock::given(method("POST"))
        .and(path("/embeddings"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [{ "embedding": [0.1, 0.2, 0.3], "index": 0 }]
        })))
        .mount(&server)
        .await;

    let ep = LlamaCppEmbeddingProvider::new("m", server.uri(), 4);
    let err = ep.embed(&["hello"]).await.unwrap_err();
    assert!(
        matches!(
            err,
            EmbeddingError::DimensionMismatch {
                expected: 4,
                actual: 3
            }
        ),
        "got {err:?}"
    );
}
