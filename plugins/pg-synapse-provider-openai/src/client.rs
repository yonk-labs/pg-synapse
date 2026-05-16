//! HTTP client for OpenAI / OpenAI-compatible Chat Completions endpoints.
//!
//! Targets the `POST {base_url}/chat/completions` shape used by:
//!
//! * OpenAI proper (`https://api.openai.com/v1`).
//! * vLLM with the OpenAI-compatible server (`http://host:port/v1`).
//! * llama-cpp-server (`./server` from llama.cpp).
//! * LM Studio's local OpenAI server.
//! * The Ollama OpenAI shim (`/v1` mount).
//!
//! Streaming is intentionally unimplemented in v0.1 (see kernel decision D8):
//! [`OpenAiProvider::stream`] returns [`LlmError::Provider`] so callers can
//! detect the missing capability without a panic.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::{Client, header};
use serde_json::{Value, json};

use pg_synapse_core::LlmProvider;
use pg_synapse_core::error::LlmError;
use pg_synapse_core::types::{
    CompletionChunk, CompletionRequest, CompletionResponse, Role, ToolCall, Usage,
};

/// Recursively sanitize a JSON Schema value so OpenAI's strict schema
/// validator accepts it.
///
/// The only known issue today is `items` being a boolean or empty object,
/// which schemars emits for `Vec<serde_json::Value>`. The rule is:
/// if `items` is not a JSON object, replace it with `{"type": "string"}`.
/// This covers the `params` bind-parameter array used by sql_exec / sql_query.
pub fn sanitize_tool_schema(v: &mut Value) {
    match v {
        Value::Object(map) => {
            if let Some(items) = map.get("items") {
                if !items.is_object() {
                    // Replace boolean schema or missing object with a permissive
                    // string schema. "string" is the safest primitive; the actual
                    // runtime accepts any JSON value via the bind-param path.
                    map.insert("items".into(), json!({"type": "string"}));
                }
            }
            for val in map.values_mut() {
                sanitize_tool_schema(val);
            }
        }
        Value::Array(arr) => {
            for val in arr.iter_mut() {
                sanitize_tool_schema(val);
            }
        }
        _ => {}
    }
}

/// Returns true when `model` is a reasoning model (gpt-5, o1, o3, o4 families).
///
/// Reasoning models have two key API differences compared with standard chat
/// completion models:
///   1. They reject any explicit `temperature` (must be omitted).
///   2. They use `max_completion_tokens` rather than `max_tokens`.
pub fn is_reasoning_model(model: &str) -> bool {
    let lower = model.to_lowercase();
    lower.starts_with("gpt-5")
        || lower.starts_with("o1")
        || lower.starts_with("o3")
        || lower.starts_with("o4")
}

/// Live provider that speaks OpenAI Chat Completions over HTTPS / HTTP.
pub struct OpenAiProvider {
    http: Client,
    base_url: String,
    api_key: Option<String>,
    model: String,
    /// Optional extra headers, e.g. `OpenAI-Organization`.
    extra_headers: Vec<(String, String)>,
    /// Optional `params.cost_per_million_tokens_in` / `cost_per_million_tokens_out`
    /// for cost computation when the server doesn't return usage cost.
    cost_per_million_in: Option<f64>,
    cost_per_million_out: Option<f64>,
}

impl OpenAiProvider {
    /// Construct a provider for `model` against `base_url`.
    ///
    /// `base_url` should be the path prefix that hosts `/chat/completions`,
    /// for example `https://api.openai.com/v1` or `http://192.168.1.193:8000/v1`.
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
            extra_headers: vec![],
            cost_per_million_in: None,
            cost_per_million_out: None,
        }
    }

    /// Attach a Bearer API key (sent as `Authorization: Bearer {key}`).
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }

    /// Configure USD-per-million-tokens pricing for prompt and completion.
    pub fn with_cost(mut self, per_million_in: Option<f64>, per_million_out: Option<f64>) -> Self {
        self.cost_per_million_in = per_million_in;
        self.cost_per_million_out = per_million_out;
        self
    }

    /// Append an extra header sent on every request.
    pub fn with_extra_header(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.extra_headers.push((k.into(), v.into()));
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
        if let Some(k) = &self.api_key {
            let v = header::HeaderValue::from_str(&format!("Bearer {k}"))
                .map_err(|_| LlmError::Auth("openai".into()))?;
            h.insert(header::AUTHORIZATION, v);
        }
        for (k, v) in &self.extra_headers {
            let name =
                header::HeaderName::from_bytes(k.as_bytes()).map_err(|_| LlmError::Provider {
                    provider: "openai".into(),
                    reason: format!("invalid header name: {k}"),
                })?;
            let val = header::HeaderValue::from_str(v).map_err(|_| LlmError::Provider {
                provider: "openai".into(),
                reason: format!("invalid header value for {k}"),
            })?;
            h.insert(name, val);
        }
        Ok(h)
    }

    fn build_payload(&self, req: &CompletionRequest, stream: bool) -> Value {
        // Convert kernel Messages to OpenAI Chat Completions format.
        // OpenAI roles: system, user, assistant, tool.
        //
        // Tool calls require:
        //   - Assistant message issuing the call:
        //       {role:"assistant", content: <opt>, tool_calls:[{id, type:"function",
        //        function:{name, arguments(string)}}]}
        //   - Tool response: {role:"tool", tool_call_id, content}
        let mut messages = Vec::<Value>::new();
        for m in &req.messages {
            let role = match m.role {
                Role::System => "system",
                Role::User => "user",
                Role::Assistant => "assistant",
                Role::Tool => "tool",
            };

            match m.role {
                Role::Tool => {
                    // Tool response: prefer structured tool_output, fall back to content.
                    let content = match (&m.tool_output, &m.content) {
                        (Some(v), _) => serde_json::to_string(v).unwrap_or_else(|_| "null".into()),
                        (None, Some(t)) => t.clone(),
                        (None, None) => "".into(),
                    };
                    let mut obj = serde_json::Map::new();
                    obj.insert("role".into(), json!(role));
                    obj.insert("content".into(), json!(content));
                    if let Some(tcid) = &m.tool_call_id {
                        obj.insert("tool_call_id".into(), json!(tcid));
                    }
                    messages.push(Value::Object(obj));
                }
                Role::Assistant if m.tool_call_id.is_some() && m.tool_name.is_some() => {
                    let args_str = m
                        .tool_input
                        .as_ref()
                        .map(|v| serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
                        .unwrap_or_else(|| "{}".into());
                    messages.push(json!({
                        "role": "assistant",
                        "content": m.content,
                        "tool_calls": [{
                            "id": m.tool_call_id.as_deref().unwrap_or(""),
                            "type": "function",
                            "function": {
                                "name": m.tool_name.as_deref().unwrap_or(""),
                                "arguments": args_str,
                            }
                        }]
                    }));
                }
                _ => {
                    messages.push(json!({
                        "role": role,
                        "content": m.content,
                    }));
                }
            }
        }

        // Model: per-request override wins; fall back to the provider's default.
        let model = match &req.model {
            Some(m) if !m.is_empty() => m.as_str(),
            _ => self.model.as_str(),
        };

        let reasoning = is_reasoning_model(model);

        let mut payload = json!({
            "model": model,
            "messages": messages,
        });

        if reasoning {
            // Reasoning models (gpt-5, o1, o3, o4 families) reject non-default
            // temperature. Omit it entirely regardless of what the caller set.
            // Use max_completion_tokens (not max_tokens) per OpenAI spec.
            // Default to 2048 when unset: reasoning models spend tokens on
            // hidden chain-of-thought; too low a budget yields empty content.
            let budget = req.max_tokens.unwrap_or(2048);
            payload["max_completion_tokens"] = json!(budget);
        } else {
            if let Some(t) = req.temperature {
                payload["temperature"] = json!(t);
            }
            if let Some(n) = req.max_tokens {
                payload["max_tokens"] = json!(n);
            }
        }
        if stream {
            payload["stream"] = json!(true);
        }

        // Map ToolDefinitions to OpenAI function-shaped tool entries.
        if !req.tools.is_empty() {
            let tools_arr: Vec<Value> = req
                .tools
                .iter()
                .map(|td| {
                    let mut func = td.to_openai_function();
                    // Sanitize the parameters schema: OpenAI gpt-5 (and strict
                    // schema validation in general) rejects array schemas where
                    // `items` is a boolean or an empty object. schemars emits
                    // `items: true` for Vec<serde_json::Value>. Replace any
                    // non-object `items` with `{"type": "string"}` (covers
                    // the common `params` bind-parameter array case).
                    sanitize_tool_schema(&mut func);
                    json!({
                        "type": "function",
                        "function": func,
                    })
                })
                .collect();
            payload["tools"] = Value::Array(tools_arr);
        }

        // Merge provider-specific extras (top_p, presence_penalty, tool_choice,
        // response_format, ...). Skip kernel-internal fields prefixed with `_`.
        if let Value::Object(extra) = &req.params
            && let Value::Object(p_obj) = &mut payload
        {
            for (k, v) in extra {
                if k.starts_with('_') {
                    continue;
                }
                p_obj.insert(k.clone(), v.clone());
            }
        }

        payload
    }

    fn compute_cost(&self, tokens_in: u32, tokens_out: u32) -> Option<f64> {
        match (self.cost_per_million_in, self.cost_per_million_out) {
            (Some(ci), Some(co)) => {
                Some((tokens_in as f64 / 1_000_000.0) * ci + (tokens_out as f64 / 1_000_000.0) * co)
            }
            _ => None,
        }
    }

    /// List models from `GET {base_url}/models`.
    ///
    /// Useful for confirming that a vLLM endpoint exposes the expected model
    /// before kicking off an execution. Returns the `data[].id` values.
    pub async fn list_models(&self) -> Result<Vec<String>, LlmError> {
        let url = format!("{}/models", self.base_url);
        let headers = self.build_headers()?;
        let resp = self
            .http
            .get(&url)
            .headers(headers)
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        let status = resp.status();
        if !status.is_success() {
            return Err(map_http_error(
                status,
                resp.text().await.unwrap_or_default(),
            ));
        }
        let body: Value = resp.json().await.map_err(|e| LlmError::Provider {
            provider: "openai".into(),
            reason: format!("invalid JSON from /models: {e}"),
        })?;
        let arr =
            body.get("data")
                .and_then(|v| v.as_array())
                .ok_or_else(|| LlmError::Provider {
                    provider: "openai".into(),
                    reason: "no data array in /models response".into(),
                })?;
        Ok(arr
            .iter()
            .filter_map(|v| v.get("id").and_then(|v| v.as_str()).map(String::from))
            .collect())
    }
}

fn map_http_error(status: reqwest::StatusCode, body: String) -> LlmError {
    match status.as_u16() {
        401 | 403 => LlmError::Auth("openai".into()),
        429 => LlmError::RateLimited {
            provider: "openai".into(),
            retry_after_ms: None,
        },
        _ => LlmError::Provider {
            provider: "openai".into(),
            reason: format!("HTTP {status}: {body}"),
        },
    }
}

#[async_trait]
impl LlmProvider for OpenAiProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        let headers = self.build_headers()?;
        let payload = self.build_payload(&req, false);

        tracing::debug!(
            target: "pg_synapse_openai",
            url = %url,
            model = %self.model,
            "openai chat completion request"
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
            provider: "openai".into(),
            reason: format!("invalid JSON: {e}"),
        })?;

        let first =
            body.get("choices")
                .and_then(|c| c.get(0))
                .ok_or_else(|| LlmError::Provider {
                    provider: "openai".into(),
                    reason: "no choices in response".into(),
                })?;
        let message = first.get("message").ok_or_else(|| LlmError::Provider {
            provider: "openai".into(),
            reason: "no message in choice[0]".into(),
        })?;
        let finish_reason = first
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();

        // Reasoning models may return content: null when they produce only
        // chain-of-thought tokens. Return Some("") rather than None so the
        // agent loop can inspect finish_reason and decide whether to retry
        // or stop, without crashing on an unexpected None content path.
        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from)
            .or_else(|| {
                if message.get("content").map(|v| v.is_null()).unwrap_or(false) {
                    Some(String::new())
                } else {
                    None
                }
            });

        let mut tool_calls = Vec::<ToolCall>::new();
        if let Some(arr) = message.get("tool_calls").and_then(|v| v.as_array()) {
            for tc in arr {
                let id = tc
                    .get("id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let func = tc.get("function").cloned().unwrap_or(Value::Null);
                let name = func
                    .get("name")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let args_str = func
                    .get("arguments")
                    .and_then(|v| v.as_str())
                    .unwrap_or("{}");
                let args: Value =
                    serde_json::from_str(args_str).unwrap_or_else(|_| json!({ "_raw": args_str }));
                tool_calls.push(ToolCall { id, name, args });
            }
        }

        let usage_obj = body.get("usage").cloned().unwrap_or(Value::Null);
        let tokens_in = usage_obj
            .get("prompt_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let tokens_out = usage_obj
            .get("completion_tokens")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let cost_usd = self.compute_cost(tokens_in, tokens_out);

        Ok(CompletionResponse {
            content,
            tool_calls,
            finish_reason,
            usage: Usage {
                tokens_in,
                tokens_out,
                cost_usd,
            },
        })
    }

    async fn stream(
        &self,
        _req: CompletionRequest,
    ) -> Result<BoxStream<'static, Result<CompletionChunk, LlmError>>, LlmError> {
        // v0.1: streaming over SQL is deferred per kernel decision D8.
        // Sync `complete` is the canonical path. Returning an error keeps the
        // caller honest rather than silently degrading.
        Err(LlmError::Provider {
            provider: "openai".into(),
            reason: "streaming not implemented in v0.1; use complete()".into(),
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use pg_synapse_core::types::Message;
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

    #[test]
    fn build_payload_uses_request_model_when_set() {
        let p = OpenAiProvider::new("default-model", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: Some("override-model".into()),
            // 0.5 is exactly representable in both f32 and f64; avoids
            // tripping on 0.7 widening to 0.6999999...
            temperature: Some(0.5),
            max_tokens: Some(64),
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["model"], "override-model");
        assert_eq!(payload["temperature"], 0.5);
        assert_eq!(payload["max_tokens"], 64);
        assert!(payload.get("stream").is_none());
    }

    #[test]
    fn build_payload_falls_back_to_default_model() {
        let p = OpenAiProvider::new("default-model", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["model"], "default-model");
    }

    #[test]
    fn build_payload_merges_extra_params_and_skips_internal() {
        let p = OpenAiProvider::new("m", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::json!({
                "top_p": 0.9,
                "_resolved_api_key": "should-not-leak",
            }),
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["top_p"], 0.9);
        assert!(payload.get("_resolved_api_key").is_none());
    }

    #[test]
    fn build_payload_renders_tool_call_message() {
        let p = OpenAiProvider::new("m", "http://x");
        let assistant_call = Message {
            execution_id: Uuid::nil(),
            seq: 1,
            role: Role::Assistant,
            content: None,
            tool_call_id: Some("call_42".into()),
            tool_name: Some("search".into()),
            tool_input: Some(serde_json::json!({ "q": "rust" })),
            tool_output: None,
            timestamp: Utc::now(),
        };
        let req = CompletionRequest {
            messages: vec![assistant_call],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        let m = &payload["messages"][0];
        assert_eq!(m["role"], "assistant");
        assert_eq!(m["tool_calls"][0]["id"], "call_42");
        assert_eq!(m["tool_calls"][0]["function"]["name"], "search");
        assert_eq!(
            m["tool_calls"][0]["function"]["arguments"],
            "{\"q\":\"rust\"}"
        );
    }

    // B1: reasoning-model payload uses max_completion_tokens, omits temperature.
    #[test]
    fn build_payload_reasoning_model_uses_max_completion_tokens() {
        let p = OpenAiProvider::new("gpt-5-mini", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("think")],
            tools: vec![],
            model: None,
            temperature: Some(0.7), // should be silently dropped
            max_tokens: Some(512),
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["max_completion_tokens"], 512);
        assert!(
            payload.get("max_tokens").is_none(),
            "max_tokens must be absent for reasoning models"
        );
        assert!(
            payload.get("temperature").is_none(),
            "temperature must be absent for reasoning models"
        );
    }

    // B1: non-reasoning model payload is unchanged (max_tokens + temperature present).
    #[test]
    fn build_payload_non_reasoning_model_unchanged() {
        let p = OpenAiProvider::new("gpt-4o", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: Some(0.5),
            max_tokens: Some(128),
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["max_tokens"], 128);
        assert_eq!(payload["temperature"], 0.5);
        assert!(
            payload.get("max_completion_tokens").is_none(),
            "max_completion_tokens must be absent for standard models"
        );
    }

    // B1: is_reasoning_model matches expected prefixes.
    #[test]
    fn reasoning_model_detection() {
        assert!(is_reasoning_model("gpt-5-mini"));
        assert!(is_reasoning_model("GPT-5"));
        assert!(is_reasoning_model("o1-preview"));
        assert!(is_reasoning_model("o3-mini"));
        assert!(is_reasoning_model("o4-mini"));
        assert!(!is_reasoning_model("gpt-4o"));
        assert!(!is_reasoning_model("gpt-3.5-turbo"));
        assert!(!is_reasoning_model("claude-3-sonnet"));
    }

    // B1: live test against gpt-5-mini (skipped without live-tests feature).
    #[cfg(feature = "live-tests")]
    #[tokio::test]
    async fn live_gpt5_mini_completes() {
        let key =
            std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY must be set for live-tests");
        let provider =
            OpenAiProvider::new("gpt-5-mini", "https://api.openai.com/v1").with_api_key(key);
        let req = CompletionRequest {
            messages: vec![user_msg("Reply with exactly the word PONG.")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: Some(2048),
            params: serde_json::Value::Null,
        };
        let resp = provider
            .complete(req)
            .await
            .expect("live completion must succeed");
        let content = resp.content.unwrap_or_default();
        assert!(!content.is_empty(), "gpt-5-mini returned empty content");
    }
}
