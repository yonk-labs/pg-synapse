//! HTTP clients for llama.cpp server (OpenAI-compatible endpoints).
//!
//! `llama-server` exposes the same Chat Completions and Embeddings wire shapes
//! as OpenAI, so the mapping logic here mirrors
//! `pg-synapse-provider-openai/src/client.rs`. The key differences are:
//!
//! * Default `base_url` is `http://127.0.0.1:8080/v1` (llama-server default).
//! * No API key is required for local servers (optional header still supported).
//! * The model field is kept for trait compatibility but llama-server ignores it
//!   (it always runs whatever model it was started with).
//! * Streaming is deferred (v0.1, kernel decision D8).
//!
//! ## Surfaces
//!
//! * [`LlamaCppProvider`]: chat completions via `POST {base_url}/chat/completions`.
//! * [`LlamaCppEmbeddingProvider`]: dense vectors via `POST {base_url}/embeddings`.

use std::time::Duration;

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::{Client, header};
use serde_json::{Value, json};

use pg_synapse_core::error::{EmbeddingError, LlmError};
use pg_synapse_core::types::{
    CompletionChunk, CompletionRequest, CompletionResponse, EmbeddingVector, Role, ToolCall, Usage,
};
use pg_synapse_core::{EmbeddingProvider, LlmProvider};

/// Default llama-server endpoint.
pub const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8080/v1";

// ---------------------------------------------------------------------------
// LlamaCppProvider (LlmProvider)
// ---------------------------------------------------------------------------

/// Live provider that speaks OpenAI Chat Completions against a llama-server.
///
/// The llama.cpp server (`llama-server`) exposes `POST /v1/chat/completions`
/// using the same request/response shape as OpenAI. Configure the endpoint via
/// the `base_url` field on the profile row or `params.base_url`.
pub struct LlamaCppProvider {
    http: Client,
    base_url: String,
    /// Optional Bearer token (most local servers do not require one).
    api_key: Option<String>,
    /// Model name stored for `model_name()` conformance; llama-server ignores
    /// the `model` field in requests and always uses its loaded GGUF.
    model: String,
}

impl LlamaCppProvider {
    /// Construct a provider for `model` against `base_url`.
    ///
    /// `base_url` should be the path prefix hosting `/chat/completions`, e.g.
    /// `http://127.0.0.1:8080/v1`. A trailing slash is stripped.
    pub fn new(model: impl Into<String>, base_url: impl Into<String>) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(120))
                .build()
                .expect("reqwest client builds with defaults"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: None,
            model: model.into(),
        }
    }

    /// Attach a Bearer API key (sent as `Authorization: Bearer {key}`).
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
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
                .map_err(|_| LlmError::Auth("llama-cpp".into()))?;
            h.insert(header::AUTHORIZATION, v);
        }
        Ok(h)
    }

    fn build_payload(&self, req: &CompletionRequest, stream: bool) -> Value {
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

        let model = match &req.model {
            Some(m) if !m.is_empty() => m.as_str(),
            _ => self.model.as_str(),
        };

        let mut payload = json!({
            "model": model,
            "messages": messages,
        });

        if let Some(t) = req.temperature {
            payload["temperature"] = json!(t);
        }
        if let Some(n) = req.max_tokens {
            payload["max_tokens"] = json!(n);
        }
        if stream {
            payload["stream"] = json!(true);
        }

        if !req.tools.is_empty() {
            let tools_arr: Vec<Value> = req
                .tools
                .iter()
                .map(|td| {
                    json!({
                        "type": "function",
                        "function": td.to_openai_function(),
                    })
                })
                .collect();
            payload["tools"] = Value::Array(tools_arr);
        }

        // Merge provider-specific extras from params; skip internal `_` fields.
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
}

fn map_http_error(status: reqwest::StatusCode, body: String) -> LlmError {
    match status.as_u16() {
        401 | 403 => LlmError::Auth("llama-cpp".into()),
        429 => LlmError::RateLimited {
            provider: "llama-cpp".into(),
            retry_after_ms: None,
        },
        _ => LlmError::Provider {
            provider: "llama-cpp".into(),
            reason: format!("HTTP {status}: {body}"),
        },
    }
}

#[async_trait]
impl LlmProvider for LlamaCppProvider {
    async fn complete(&self, req: CompletionRequest) -> Result<CompletionResponse, LlmError> {
        let url = format!("{}/chat/completions", self.base_url);
        let headers = self.build_headers()?;
        let payload = self.build_payload(&req, false);

        tracing::debug!(
            target: "pg_synapse_llama_cpp",
            url = %url,
            model = %self.model,
            "llama-cpp chat completion request"
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
            provider: "llama-cpp".into(),
            reason: format!("invalid JSON: {e}"),
        })?;

        let first =
            body.get("choices")
                .and_then(|c| c.get(0))
                .ok_or_else(|| LlmError::Provider {
                    provider: "llama-cpp".into(),
                    reason: "no choices in response".into(),
                })?;
        let message = first.get("message").ok_or_else(|| LlmError::Provider {
            provider: "llama-cpp".into(),
            reason: "no message in choice[0]".into(),
        })?;
        let finish_reason = first
            .get("finish_reason")
            .and_then(|v| v.as_str())
            .unwrap_or("stop")
            .to_string();

        let content = message
            .get("content")
            .and_then(|v| v.as_str())
            .map(String::from);

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
        Err(LlmError::Provider {
            provider: "llama-cpp".into(),
            reason: "streaming not implemented in v0.1; use complete()".into(),
        })
    }

    fn model_name(&self) -> &str {
        &self.model
    }
}

// ---------------------------------------------------------------------------
// LlamaCppEmbeddingProvider (EmbeddingProvider)
// ---------------------------------------------------------------------------

/// Embedding provider that calls `POST {base_url}/embeddings` on a llama-server.
///
/// The request shape is `{"input": [...], "model": "..."}` and the response
/// is `{"data": [{"embedding": [...]}]}`, identical to the OpenAI Embeddings
/// API that llama-server implements.
pub struct LlamaCppEmbeddingProvider {
    http: Client,
    base_url: String,
    model: String,
    dimension: usize,
    api_key: Option<String>,
}

impl LlamaCppEmbeddingProvider {
    /// Construct an embedding provider.
    ///
    /// `dimension` must match the output dimension of the GGUF model loaded by
    /// llama-server. If the server returns a vector of a different length,
    /// [`EmbeddingError::DimensionMismatch`] is returned per batch.
    pub fn new(model: impl Into<String>, base_url: impl Into<String>, dimension: usize) -> Self {
        Self {
            http: Client::builder()
                .timeout(Duration::from_secs(60))
                .build()
                .expect("reqwest client builds with defaults"),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            model: model.into(),
            dimension,
            api_key: None,
        }
    }

    /// Attach a Bearer API key.
    pub fn with_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key = Some(key.into());
        self
    }
}

#[async_trait]
impl EmbeddingProvider for LlamaCppEmbeddingProvider {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbeddingError> {
        let url = format!("{}/embeddings", self.base_url);

        let mut headers = header::HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        if let Some(k) = &self.api_key {
            let v = header::HeaderValue::from_str(&format!("Bearer {k}"))
                .map_err(|_| EmbeddingError::Provider("invalid api key".into()))?;
            headers.insert(header::AUTHORIZATION, v);
        }

        let body = json!({
            "input": texts,
            "model": self.model,
        });

        let resp = self
            .http
            .post(&url)
            .headers(headers)
            .json(&body)
            .send()
            .await
            .map_err(|e| EmbeddingError::Network(e.to_string()))?;

        let status = resp.status();
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(EmbeddingError::Provider(format!("HTTP {status}: {text}")));
        }

        let parsed: Value = resp
            .json()
            .await
            .map_err(|e| EmbeddingError::Provider(format!("invalid JSON: {e}")))?;

        let data = parsed
            .get("data")
            .and_then(|v| v.as_array())
            .ok_or_else(|| EmbeddingError::Provider("no data array in response".into()))?;

        let mut out = Vec::with_capacity(data.len());
        for (i, item) in data.iter().enumerate() {
            let arr = item
                .get("embedding")
                .and_then(|v| v.as_array())
                .ok_or_else(|| {
                    EmbeddingError::Provider(format!("no embedding array at data[{i}]"))
                })?;

            let floats: Vec<f32> = arr
                .iter()
                .map(|v| v.as_f64().unwrap_or(0.0) as f32)
                .collect();

            if floats.len() != self.dimension {
                return Err(EmbeddingError::DimensionMismatch {
                    expected: self.dimension,
                    actual: floats.len(),
                });
            }

            out.push(EmbeddingVector::new(floats));
        }

        Ok(out)
    }

    fn dimension(&self) -> usize {
        self.dimension
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
        let p = LlamaCppProvider::new("default-model", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: Some("override-model".into()),
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
        let p = LlamaCppProvider::new("local-granite", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::Value::Null,
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["model"], "local-granite");
    }

    #[test]
    fn build_payload_skips_internal_params() {
        let p = LlamaCppProvider::new("m", "http://x");
        let req = CompletionRequest {
            messages: vec![user_msg("hi")],
            tools: vec![],
            model: None,
            temperature: None,
            max_tokens: None,
            params: serde_json::json!({
                "top_p": 0.9,
                "_internal": "should-not-leak",
            }),
        };
        let payload = p.build_payload(&req, false);
        assert_eq!(payload["top_p"], 0.9);
        assert!(payload.get("_internal").is_none());
    }

    #[test]
    fn embedding_provider_dimension_is_reported() {
        let ep = LlamaCppEmbeddingProvider::new("nomic-embed", "http://x", 768);
        assert_eq!(ep.dimension(), 768);
        assert_eq!(ep.model_name(), "nomic-embed");
    }

    #[test]
    fn base_url_strips_trailing_slash() {
        let p = LlamaCppProvider::new("m", "http://127.0.0.1:8080/v1/");
        assert_eq!(p.base_url(), "http://127.0.0.1:8080/v1");
    }
}
