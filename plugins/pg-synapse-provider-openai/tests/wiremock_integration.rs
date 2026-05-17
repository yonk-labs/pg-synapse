//! Integration tests for [`pg_synapse_provider_openai::OpenAiProvider`] backed
//! by [wiremock]. These do not hit the network: each test spins up a local
//! mock server that mimics the OpenAI Chat Completions / Models wire shape.

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pg_synapse_core::LlmProvider;
use pg_synapse_core::LlmProviderFactory;
use pg_synapse_core::error::LlmError;
use pg_synapse_core::types::{
    CompletionRequest, LlmProfileRow, Message, Role, ToolDefinition, ToolSchema,
};
use pg_synapse_provider_openai::{OpenAiProvider, OpenAiProviderFactory};

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

#[tokio::test]
async fn complete_returns_text_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{
                "message": { "role": "assistant", "content": "Hello!" },
                "finish_reason": "stop"
            }],
            "usage": { "prompt_tokens": 12, "completion_tokens": 4, "total_tokens": 16 }
        })))
        .mount(&server)
        .await;

    let p = OpenAiProvider::new("gpt-test", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("Hi")],
        tools: vec![],
        model: Some("gpt-test".into()),
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("Hello!"));
    assert_eq!(resp.finish_reason, "stop");
    assert_eq!(resp.usage.tokens_in, 12);
    assert_eq!(resp.usage.tokens_out, 4);
    assert!(resp.tool_calls.is_empty());
}

#[tokio::test]
async fn complete_computes_cost_when_pricing_provided() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "choices": [{"message": {"role":"assistant","content":"ok"}, "finish_reason":"stop"}],
            "usage": {"prompt_tokens": 1_000_000, "completion_tokens": 2_000_000}
        })))
        .mount(&server)
        .await;

    // $1 per M prompt, $2 per M completion -> expected $5.00.
    let p = OpenAiProvider::new("gpt-test", server.uri()).with_cost(Some(1.0), Some(2.0));
    let resp = p
        .complete(CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .unwrap();
    assert_eq!(resp.usage.cost_usd, Some(5.0));
}

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
                        "id": "call_1",
                        "type": "function",
                        "function": { "name": "search", "arguments": "{\"q\":\"rust\"}" }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": { "prompt_tokens": 9, "completion_tokens": 7, "total_tokens": 16 }
        })))
        .mount(&server)
        .await;

    let p = OpenAiProvider::new("gpt-test", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("search the web")],
        tools: vec![ToolDefinition {
            name: "search".into(),
            description: "Web search".into(),
            schema: ToolSchema::default(),
        }],
        model: Some("gpt-test".into()),
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.unwrap();
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "call_1");
    assert_eq!(resp.tool_calls[0].name, "search");
    assert_eq!(resp.tool_calls[0].args, json!({"q":"rust"}));
    assert_eq!(resp.finish_reason, "tool_calls");
}

#[tokio::test]
async fn complete_maps_401_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(401))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("gpt-test", server.uri()).with_api_key("bad");
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
    assert!(matches!(err, LlmError::Auth(_)), "got {err:?}");
}

#[tokio::test]
async fn complete_maps_429_to_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("gpt-test", server.uri());
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
        matches!(err, LlmError::RateLimited { ref provider, .. } if provider == "openai"),
        "got {err:?}"
    );
}

#[tokio::test]
async fn complete_429_parses_retry_after_header_into_ms() {
    // Locks the PS-2a follow-up: a Retry-After header on a 429 must reach
    // LlmError::RateLimited.retry_after_ms (RetryProvider already honors it).
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "2"))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("gpt-test", server.uri());
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
        LlmError::RateLimited {
            provider,
            retry_after_ms,
        } => {
            assert_eq!(provider, "openai");
            assert_eq!(retry_after_ms, Some(2000), "2 seconds -> 2000ms");
        }
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_429_without_retry_after_header_is_none() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("gpt-test", server.uri());
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
        LlmError::RateLimited {
            retry_after_ms, ..
        } => assert_eq!(retry_after_ms, None),
        other => panic!("expected RateLimited, got {other:?}"),
    }
}

#[tokio::test]
async fn complete_maps_500_to_provider_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("gpt-test", server.uri());
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
            assert_eq!(provider, "openai");
            assert!(reason.contains("500"));
            assert!(reason.contains("boom"));
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}

#[tokio::test]
async fn list_models_returns_ids() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "data": [
                { "id": "granite-3.1-2b-instruct" },
                { "id": "another-model" }
            ]
        })))
        .mount(&server)
        .await;
    let p = OpenAiProvider::new("m", server.uri());
    let models = p.list_models().await.expect("list_models ok");
    assert_eq!(models, vec!["granite-3.1-2b-instruct", "another-model"]);
}

#[tokio::test]
async fn factory_builds_provider_with_base_url_from_profile() {
    let f = OpenAiProviderFactory;
    let p = LlmProfileRow {
        name: "vllm-test".into(),
        provider: "openai".into(),
        model: "granite-3.1-2b-instruct".into(),
        api_key_secret: None,
        base_url: Some("http://192.168.1.193:8000/v1".into()),
        params: serde_json::json!({
            "cost_per_million_tokens_in": 0.0,
            "cost_per_million_tokens_out": 0.0
        }),
    };
    let provider = f.build(p).expect("factory build ok");
    assert_eq!(provider.model_name(), "granite-3.1-2b-instruct");
}

#[tokio::test]
async fn stream_returns_unimplemented_error_in_v01() {
    let p = OpenAiProvider::new("m", "http://example.invalid");
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
            assert!(reason.contains("streaming not implemented"));
        }
        other => panic!("expected Provider error, got {other:?}"),
    }
}
