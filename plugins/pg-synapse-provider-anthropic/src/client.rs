//! HTTP client for the Anthropic Messages API.
//!
//! Targets `POST {base_url}/v1/messages` as documented at
//! <https://docs.anthropic.com/en/api/messages>.
//!
//! Key wire-format differences from OpenAI Chat Completions:
//!
//! * Authentication: `x-api-key` header (not `Authorization: Bearer`).
//! * Required versioning header: `anthropic-version: 2023-06-01`.
//! * System messages are extracted from the message list and sent as a
//!   top-level `"system"` string (concatenated when multiple are present).
//! * `max_tokens` is required by the API; this client defaults to 4096.
//! * Tool definitions use `input_schema` (JSON Schema object) rather than
//!   `parameters`.
//! * Tool calls in the response are content blocks of type `"tool_use"`.
//! * Tool results are sent back as user-role messages with a `"tool_result"`
//!   content block (not a separate `"tool"` role as in OpenAI).
//!
//! Streaming is intentionally unimplemented in v0.1 (kernel decision D8).

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::{Client, header};
use serde_json::{Value, json};

use pg_synapse_core::LlmProvider;
use pg_synapse_core::error::LlmError;
use pg_synapse_core::llm::ProviderCapabilities;
use pg_synapse_core::types::{
    CompletionChunk, CompletionRequest, CompletionResponse, Role, ToolCall, Usage,
};

/// Anthropic Messages API version header value required by all requests.
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Default `max_tokens` sent when the caller does not specify one.
/// The Anthropic API requires this field; 4096 is a safe default for all
/// currently-available Claude models.
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// Live provider that speaks the Anthropic Messages API over HTTPS.
pub struct AnthropicProvider {
    http: Client,
    /// Base URL, trailing slash stripped. Default: `https://api.anthropic.com`.
    base_url: String,
    /// API key sent as `x-api-key`.
    api_key: Option<String>,
    /// Default model (e.g. `"claude-3-5-haiku-20241022"`).
    model: String,
    /// When true, attach `"cache_control": {"type": "ephemeral"}` to the last
    /// system block so the prompt is eligible for prompt caching.
    prompt_caching: bool,
}

impl AnthropicProvider {
    /// Construct a provider for `model` against `base_url`.
    ///
    /// `base_url` should be the root URL without a path suffix, for example
    /// `https://api.anthropic.com` (the default) or a custom proxy. The
    /// `/v1/messages` path is appended by [`AnthropicProvider::complete`].
    ///
    /// A trailing slash on `base_url` is stripped.
    pub fn new(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client builds with defaults"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: None,
            model: model.into(),
            prompt_caching: false,
        }
    }

    /// Attach the Anthropic API key (sent as `x-api-key: {key}`).
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Enable prompt caching: attaches `cache_control: ephemeral` to the
    /// system block so Anthropic can cache it across requests.
    pub fn with_prompt_caching(mut self, enabled: bool) -> Self {
        self.prompt_caching = enabled;
        self
    }

    /// Borrow the configured base URL (trailing slash stripped).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn build_headers(&self) -> Result<header::HeaderMap, LlmError> {
        let mut h = header::HeaderMap::new();
        h.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        h.insert(
            header::HeaderName::from_static("anthropic-version"),
            header::HeaderValue::from_static(ANTHROPIC_VERSION),
        );
        if let Some(k) = &self.api_key {
            let v =
                header::HeaderValue::from_str(k).map_err(|_| LlmError::Auth("anthropic".into()))?;
            h.insert(header::HeaderName::from_static("x-api-key"), v);
        }
        Ok(h)
    }

    /// Build the JSON body for a Messages API request.
    ///
    /// The Anthropic wire format differs from OpenAI in several ways.
    /// System messages are extracted into a top-level `"system"` field.
    /// Tool results use a `"tool_result"` content block inside a user message,
    /// not a standalone `"tool"` role message. Assistant tool calls are content
    /// blocks of type `"tool_use"`, not a top-level `tool_calls` array.
    ///
    /// Exposed as `pub` so integration tests can inspect the outgoing body shape
    /// without needing a live HTTP server.
    pub fn build_payload(&self, req: &CompletionRequest) -> Value {
        // Collect system text first (concatenate multiple System-role messages).
        let mut system_parts: Vec<String> = Vec::new();
        let mut messages: Vec<Value> = Vec::new();

        for m in &req.messages {
            match m.role {
                Role::System => {
                    if let Some(text) = &m.content {
                        system_parts.push(text.clone());
                    }
                }
                Role::User => {
                    // Could be a plain user message or a tool result.
                    if let Some(tcid) = &m.tool_call_id {
                        // Tool result: send as user message with tool_result block.
                        let content_val = match (&m.tool_output, &m.content) {
                            (Some(v), _) => {
                                // Anthropic expects string content in the block.
                                serde_json::to_string(v).unwrap_or_else(|_| "null".into())
                            }
                            (None, Some(t)) => t.clone(),
                            (None, None) => String::new(),
                        };
                        messages.push(json!({
                            "role": "user",
                            "content": [{
                                "type": "tool_result",
                                "tool_use_id": tcid,
                                "content": content_val,
                            }]
                        }));
                    } else {
                        messages.push(json!({
                            "role": "user",
                            "content": m.content.as_deref().unwrap_or(""),
                        }));
                    }
                }
                Role::Assistant => {
                    // Assistant may be a text turn or a tool_use turn.
                    if m.tool_call_id.is_some() && m.tool_name.is_some() {
                        // Assistant issued a tool call.
                        let input = m.tool_input.clone().unwrap_or_else(|| json!({}));
                        let id = m.tool_call_id.as_deref().unwrap_or("").to_string();
                        let name = m.tool_name.as_deref().unwrap_or("").to_string();

                        let mut blocks: Vec<Value> = Vec::new();
                        // Include any text content alongside the tool call block.
                        if let Some(text) = &m.content {
                            if !text.is_empty() {
                                blocks.push(json!({"type": "text", "text": text}));
                            }
                        }
                        blocks.push(json!({
                            "type": "tool_use",
                            "id": id,
                            "name": name,
                            "input": input,
                        }));
                        messages.push(json!({"role": "assistant", "content": blocks}));
                    } else {
                        messages.push(json!({
                            "role": "assistant",
                            "content": m.content.as_deref().unwrap_or(""),
                        }));
                    }
                }
                // Tool role is mapped to a user message with tool_result block above;
                // handle it identically in case the role is set explicitly.
                Role::Tool => {
                    let tcid = m.tool_call_id.as_deref().unwrap_or("").to_string();
                    let content_val = match (&m.tool_output, &m.content) {
                        (Some(v), _) => serde_json::to_string(v).unwrap_or_else(|_| "null".into()),
                        (None, Some(t)) => t.clone(),
                        (None, None) => String::new(),
                    };
                    messages.push(json!({
                        "role": "user",
                        "content": [{
                            "type": "tool_result",
                            "tool_use_id": tcid,
                            "content": content_val,
                        }]
                    }));
                }
            }
        }

        // Model: per-request override wins; fall back to the provider's default.
        let model = match &req.model {
            Some(m) if !m.is_empty() => m.as_str(),
            _ => self.model.as_str(),
        };

        let max_tokens = req.max_tokens.unwrap_or(DEFAULT_MAX_TOKENS);

        let mut payload = json!({
            "model": model,
            "max_tokens": max_tokens,
            "messages": messages,
        });

        // System block: build as text content block so we can attach
        // cache_control when prompt caching is enabled.
        if !system_parts.is_empty() {
            let system_text = system_parts.join("\n\n");
            if self.prompt_caching {
                // Anthropic prompt caching: mark the system block as ephemeral
                // so it is eligible for caching on repeated calls with the same text.
                payload["system"] = json!([{
                    "type": "text",
                    "text": system_text,
                    "cache_control": { "type": "ephemeral" },
                }]);
            } else {
                // Simple string form is fine when caching is not needed.
                payload["system"] = json!(system_text);
            }
        }

        if let Some(t) = req.temperature {
            payload["temperature"] = json!(t);
        }

        // Map ToolDefinitions to Anthropic's tool format.
        // Anthropic uses `input_schema` (a JSON Schema object) rather than
        // OpenAI's `parameters` field.
        if !req.tools.is_empty() {
            let tools_arr: Vec<Value> = req
                .tools
                .iter()
                .map(|td| {
                    let schema = serde_json::to_value(td.schema.as_root_schema())
                        .unwrap_or_else(|_| json!({"type": "object"}));
                    json!({
                        "name": td.name,
                        "description": td.description,
                        "input_schema": schema,
                    })
                })
                .collect();
            payload["tools"] = Value::Array(tools_arr);
        }

        // Merge provider-specific extras from params, skipping internal
        // fields prefixed with `_` so API keys never leak into the body.
        if let Value::Object(extra) = &req.params
            && let Value::Object(p_obj) = &mut payload
        {
            for (k, v) in extra {
                if k.starts_with('_') {
                    continue;
                }
                // prompt_caching is consumed above; skip it here.
                if k == "prompt_caching" {
                    continue;
                }
                // base_url and api_key are factory-level; don't echo them.
                if k == "base_url" || k == "api_key" {
                    continue;
                }
                p_obj.insert(k.clone(), v.clone());
            }
        }

        payload
    }
}

fn map_http_error(status: reqwest::StatusCode, body: String) -> LlmError {
    match status.as_u16() {
        401 | 403 => LlmError::Auth("anthropic".into()),
        429 => LlmError::RateLimited {
            provider: "anthropic".into(),
            retry_after_ms: None,
        },
        _ => LlmError::Provider {
            provider: "anthropic".into(),
            reason: format!("HTTP {status}: {body}"),
        },
    }
}

#[async_trait]
impl LlmProvider for AnthropicProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let url = format!("{}/v1/messages", self.base_url);
        let headers = self.build_headers()?;
        let payload = self.build_payload(&req);

        tracing::debug!(
            target: "pg_synapse_anthropic",
            url = %url,
            model = %self.model,
            "anthropic messages request"
        );

        let resp = self
            .http
            .post(&url)
            .headers(headers)
            .json(&payload)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(map_http_error(status, body));
        }

        let body: Value = resp.json().await.map_err(|e| LlmError::Provider {
            provider: "anthropic".into(),
            reason: format!("invalid JSON: {e}"),
        })?;

        // Parse the content block array.
        // Types in the array: "text" and "tool_use".
        let content_arr = body
            .get("content")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        let mut text_parts: Vec<String> = Vec::new();
        let mut tool_calls: Vec<ToolCall> = Vec::new();

        for block in &content_arr {
            let block_type = block.get("type").and_then(|v| v.as_str()).unwrap_or("");
            match block_type {
                "text" => {
                    if let Some(t) = block.get("text").and_then(|v| v.as_str()) {
                        text_parts.push(t.to_string());
                    }
                }
                "tool_use" => {
                    let id = block
                        .get("id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let name = block
                        .get("name")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    // `input` is already a JSON object in the Anthropic response.
                    let args = block.get("input").cloned().unwrap_or_else(|| json!({}));
                    tool_calls.push(ToolCall { id, name, args });
                }
                _ => {}
            }
        }

        let content = if text_parts.is_empty() {
            None
        } else {
            Some(text_parts.join(""))
        };

        let finish_reason = body
            .get("stop_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("end_turn")
            .to_string();

        let usage_obj = body.get("usage").cloned().unwrap_or(Value::Null);
        let tokens_in = usage_obj
            .get("input_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let tokens_out = usage_obj
            .get("output_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        Ok(CompletionResponse {
            content,
            tool_calls,
            finish_reason,
            usage: Usage {
                tokens_in,
                tokens_out,
                cost_usd: None,
            },
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        // v0.1: streaming is deferred per kernel decision D8.
        // Returning an error keeps callers honest rather than silently degrading.
        Err(LlmError::Provider {
            provider: "anthropic".into(),
            reason: "streaming not implemented in v0.1; use complete()".into(),
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }

    fn capabilities(&self) -> ProviderCapabilities {
        ProviderCapabilities {
            tool_use: true,
            streaming: true,
            json_mode: true,
            vision: true,
            max_context_tokens: None,
            max_output_tokens: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pg_synapse_core::types::{Message, ToolDefinition, ToolSchema};
    use uuid::Uuid;

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

    #[test]
    fn build_payload_extracts_system_message() {
        let p = AnthropicProvider::new("claude-3-5-haiku-20241022", "http://x");
        let req = CompletionRequest {
            messages: vec![system_msg("Be brief."), user_msg("Hello")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        assert_eq!(payload["system"], "Be brief.");
        let msgs = payload["messages"].as_array().unwrap();
        // Only the user message should be in the messages array.
        assert_eq!(msgs.len(), 1);
        assert_eq!(msgs[0]["role"], "user");
    }

    #[test]
    fn build_payload_uses_request_model_override() {
        let p = AnthropicProvider::new("default-model", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: Some("claude-opus-4-5".into()),
            temperature: Some(0.5),
            max_tokens: Some(64),
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        assert_eq!(payload["model"], "claude-opus-4-5");
        assert_eq!(payload["max_tokens"], 64);
        assert_eq!(payload["temperature"], 0.5);
    }

    #[test]
    fn build_payload_defaults_max_tokens_to_4096() {
        let p = AnthropicProvider::new("m", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        assert_eq!(payload["max_tokens"], 4096);
    }

    #[test]
    fn build_payload_maps_tool_definitions_to_anthropic_shape() {
        let p = AnthropicProvider::new("m", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![ToolDefinition {
                name: "search".into(),
                description: "Search the web".into(),
                schema: ToolSchema::default(),
            }],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        let tools = payload["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "search");
        assert_eq!(tools[0]["description"], "Search the web");
        // Anthropic uses input_schema, not parameters.
        assert!(tools[0].get("input_schema").is_some());
        assert!(tools[0].get("parameters").is_none());
    }

    #[test]
    fn build_payload_prompt_caching_attaches_cache_control() {
        let p = AnthropicProvider::new("m", "http://x").with_prompt_caching(true);
        let req = CompletionRequest {
            messages: vec![system_msg("You are a helpful assistant."), user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        let system = &payload["system"];
        // When caching is enabled, system is an array of content blocks.
        assert!(
            system.is_array(),
            "system should be array when caching enabled"
        );
        let block = &system[0];
        assert_eq!(block["type"], "text");
        assert_eq!(block["cache_control"]["type"], "ephemeral");
    }

    #[test]
    fn build_payload_skips_internal_params() {
        let p = AnthropicProvider::new("m", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::json!({
                "top_p": 0.9,
                "_resolved_api_key": "secret",
                "api_key": "also-secret",
                "base_url": "http://other",
            }),
        };
        let payload = p.build_payload(&req);
        assert_eq!(payload["top_p"], 0.9);
        assert!(payload.get("_resolved_api_key").is_none());
        assert!(payload.get("api_key").is_none());
        assert!(payload.get("base_url").is_none());
    }

    #[test]
    fn build_payload_tool_result_from_tool_role() {
        let p = AnthropicProvider::new("m", "http://x");
        let tool_result = Message {
            execution_id: Uuid::nil(),
            seq: 2,
            role: Role::Tool,
            content: None,
            tool_call_id: Some("call_abc".into()),
            tool_name: Some("search".into()),
            tool_input: None,
            tool_output: Some(serde_json::json!({"results": ["a", "b"]})),
            timestamp: Utc::now(),
        };
        let req = CompletionRequest {
            messages: vec![tool_result],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req);
        let msgs = payload["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "user");
        let block = &msgs[0]["content"][0];
        assert_eq!(block["type"], "tool_result");
        assert_eq!(block["tool_use_id"], "call_abc");
    }
}
