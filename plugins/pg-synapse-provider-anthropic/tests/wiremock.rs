//! Integration tests for [`pg_synapse_provider_anthropic::AnthropicProvider`]
//! backed by [wiremock]. These do not hit the network: each test spins up a
//! local mock server that mimics the Anthropic Messages API wire shape.

use chrono::Utc;
use serde_json::json;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use pg_synapse_core::LlmProvider;
use pg_synapse_core::LlmProviderFactory;
use pg_synapse_core::error::LlmError;
use pg_synapse_core::types::{
    CompletionRequest, LlmProfileRow, Message, Role, ToolDefinition, ToolSchema,
};
use pg_synapse_provider_anthropic::{AnthropicProvider, AnthropicProviderFactory};

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

fn system_msg(text: &str) -> Message {
    Message {
        execution_id: Uuid::nil(),
        seq: 0,
        role: Role::System,
        content: Some(text.into()),
        tool_call_id: None,
        tool_name: None,
        tool_input: None,
        tool_output: None,
        timestamp: Utc::now(),
    }
}

/// Build a minimal Anthropic Messages success response.
fn ok_text_response(text: &str, tokens_in: u64, tokens_out: u64) -> serde_json::Value {
    json!({
        "id": "msg_01test",
        "type": "message",
        "role": "assistant",
        "model": "claude-3-5-haiku-20241022",
        "content": [{"type": "text", "text": text}],
        "stop_reason": "end_turn",
        "stop_sequence": null,
        "usage": {
            "input_tokens": tokens_in,
            "output_tokens": tokens_out
        }
    })
}

// ---------------------------------------------------------------------------
// Test 1: text completion returns content and usage.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_returns_text_and_usage() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_text_response(
            "Hello from Claude!",
            10,
            5,
        )))
        .mount(&server)
        .await;

    let p = AnthropicProvider::new("claude-3-5-haiku-20241022", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("Hi")],
        tools: vec![],
        model: None,
        temperature: None,
        max_tokens: Some(64),
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("Hello from Claude!"));
    assert_eq!(resp.finish_reason, "end_turn");
    assert_eq!(resp.usage.tokens_in, 10);
    assert_eq!(resp.usage.tokens_out, 5);
    assert!(resp.tool_calls.is_empty());
}

// ---------------------------------------------------------------------------
// Test 2: tool_use response block is parsed into ToolCall.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_parses_tool_use_response() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": "msg_02test",
            "type": "message",
            "role": "assistant",
            "model": "claude-3-5-haiku-20241022",
            "content": [{
                "type": "tool_use",
                "id": "toolu_01test",
                "name": "search",
                "input": { "query": "rust async" }
            }],
            "stop_reason": "tool_use",
            "usage": { "input_tokens": 20, "output_tokens": 10 }
        })))
        .mount(&server)
        .await;

    let p = AnthropicProvider::new("claude-3-5-haiku-20241022", server.uri());
    let req = CompletionRequest {
        messages: vec![user_msg("search for rust async")],
        tools: vec![ToolDefinition {
            name: "search".into(),
            description: "Search".into(),
            schema: ToolSchema::default(),
        }],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.tool_calls.len(), 1);
    assert_eq!(resp.tool_calls[0].id, "toolu_01test");
    assert_eq!(resp.tool_calls[0].name, "search");
    assert_eq!(resp.tool_calls[0].args, json!({"query": "rust async"}));
    assert_eq!(resp.finish_reason, "tool_use");
    assert!(resp.content.is_none());
}

// ---------------------------------------------------------------------------
// Test 3: tool_result round-trip. Assert the outgoing body has correct blocks.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tool_result_round_trip_sends_correct_body_shape() {
    let server = MockServer::start().await;

    // The outgoing request must contain a messages array.
    // We verify the body shape via build_payload below; here we just need
    // the mock to respond so the HTTP path is exercised.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_text_response(
            "Got the result",
            30,
            8,
        )))
        .mount(&server)
        .await;

    let p = AnthropicProvider::new("claude-3-5-haiku-20241022", server.uri());

    // Simulate: assistant issued tool call, then tool responded.
    let assistant_call = Message {
        execution_id: Uuid::nil(),
        seq: 1,
        role: Role::Assistant,
        content: None,
        tool_call_id: Some("toolu_99".into()),
        tool_name: Some("lookup".into()),
        tool_input: Some(json!({"id": 42})),
        tool_output: None,
        timestamp: Utc::now(),
    };
    let tool_result = Message {
        execution_id: Uuid::nil(),
        seq: 2,
        role: Role::Tool,
        content: None,
        tool_call_id: Some("toolu_99".into()),
        tool_name: Some("lookup".into()),
        tool_input: None,
        tool_output: Some(json!({"name": "Rust"})),
        timestamp: Utc::now(),
    };

    let req = CompletionRequest {
        messages: vec![user_msg("look up 42"), assistant_call, tool_result],
        tools: vec![],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let resp = p.complete(req).await.expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("Got the result"));

    // Verify body shape directly via the payload builder (unit-level check).
    let p2 = AnthropicProvider::new("m", "http://x");
    let assistant_call2 = Message {
        execution_id: Uuid::nil(),
        seq: 1,
        role: Role::Assistant,
        content: None,
        tool_call_id: Some("toolu_99".into()),
        tool_name: Some("lookup".into()),
        tool_input: Some(json!({"id": 42})),
        tool_output: None,
        timestamp: Utc::now(),
    };
    let tool_result2 = Message {
        execution_id: Uuid::nil(),
        seq: 2,
        role: Role::Tool,
        content: None,
        tool_call_id: Some("toolu_99".into()),
        tool_name: None,
        tool_input: None,
        tool_output: Some(json!({"name": "Rust"})),
        timestamp: Utc::now(),
    };
    let req2 = CompletionRequest {
        messages: vec![assistant_call2, tool_result2],
        tools: vec![],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let payload = p2.build_payload(&req2);
    let msgs = payload["messages"].as_array().unwrap();
    // First message: assistant tool_use block.
    assert_eq!(msgs[0]["role"], "assistant");
    let blocks = msgs[0]["content"].as_array().unwrap();
    assert_eq!(blocks[0]["type"], "tool_use");
    assert_eq!(blocks[0]["id"], "toolu_99");
    assert_eq!(blocks[0]["name"], "lookup");
    // Second message: user tool_result block.
    assert_eq!(msgs[1]["role"], "user");
    let result_block = &msgs[1]["content"][0];
    assert_eq!(result_block["type"], "tool_result");
    assert_eq!(result_block["tool_use_id"], "toolu_99");
}

// ---------------------------------------------------------------------------
// Test 4: 401 maps to LlmError::Auth.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_maps_401_to_auth_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(401).set_body_json(json!({
            "type": "error",
            "error": { "type": "authentication_error", "message": "invalid key" }
        })))
        .mount(&server)
        .await;

    let p = AnthropicProvider::new("m", server.uri()).with_api_key("bad-key");
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

// ---------------------------------------------------------------------------
// Test 5: 429 maps to LlmError::RateLimited.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn complete_maps_429_to_rate_limited() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(429))
        .mount(&server)
        .await;

    let p = AnthropicProvider::new("m", server.uri());
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
        matches!(err, LlmError::RateLimited { ref provider, .. } if provider == "anthropic"),
        "got {err:?}"
    );
}

// ---------------------------------------------------------------------------
// Test 6: factory builds provider using base_url from profile column.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn factory_builds_with_base_url_from_profile() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_text_response("ok", 5, 3)))
        .mount(&server)
        .await;

    let f = AnthropicProviderFactory;
    let profile = LlmProfileRow {
        name: "claude-test".into(),
        provider: "anthropic".into(),
        model: "claude-3-5-haiku-20241022".into(),
        api_key_secret: None,
        base_url: Some(server.uri()),
        params: serde_json::json!({}),
    };
    let provider = f.build(profile).expect("factory build ok");
    assert_eq!(provider.model_name(), "claude-3-5-haiku-20241022");

    let resp = provider
        .complete(CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("ok"));
}

// ---------------------------------------------------------------------------
// Test 7: prompt_caching adds cache_control to the system block.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn prompt_caching_param_adds_cache_control_block() {
    let server = MockServer::start().await;

    // Capture the request to inspect it.
    Mock::given(method("POST"))
        .and(path("/v1/messages"))
        .and(header("x-api-key", "test-key"))
        .respond_with(ResponseTemplate::new(200).set_body_json(ok_text_response(
            "cached response",
            100,
            20,
        )))
        .mount(&server)
        .await;

    let f = AnthropicProviderFactory;
    let profile = LlmProfileRow {
        name: "caching-test".into(),
        provider: "anthropic".into(),
        model: "claude-3-5-haiku-20241022".into(),
        api_key_secret: None,
        base_url: Some(server.uri()),
        params: serde_json::json!({
            "_resolved_api_key": "test-key",
            "prompt_caching": true,
        }),
    };
    let provider = f.build(profile).expect("factory build ok");

    let resp = provider
        .complete(CompletionRequest {
            messages: vec![
                system_msg("You are a helpful assistant."),
                user_msg("hello"),
            ],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        })
        .await
        .expect("complete ok");
    assert_eq!(resp.content.as_deref(), Some("cached response"));

    // Unit-level: verify payload shape has the cache_control block.
    let caching_provider = AnthropicProvider::new("m", "http://x").with_prompt_caching(true);
    let req = CompletionRequest {
        messages: vec![system_msg("You are helpful."), user_msg("hi")],
        tools: vec![],
        model: None,
        temperature: None,
        max_tokens: None,
        params: serde_json::Value::Null,
    };
    let payload = caching_provider.build_payload(&req);
    let system = &payload["system"];
    assert!(
        system.is_array(),
        "system must be array when caching enabled"
    );
    let block = &system[0];
    assert_eq!(block["cache_control"]["type"], "ephemeral");
}

// ---------------------------------------------------------------------------
// Test 8: stream() returns NotImplemented-style error in v0.1.
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stream_returns_unimplemented_error_in_v01() {
    let p = AnthropicProvider::new("m", "http://example.invalid");
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
