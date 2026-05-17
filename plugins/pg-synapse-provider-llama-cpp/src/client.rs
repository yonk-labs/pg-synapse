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
// Tool-call extraction from content (Fix B6)
// ---------------------------------------------------------------------------

/// Try to extract tool calls from a content string when the serving stack did
/// not populate `tool_calls` in the response.
///
/// llama-cpp-python (and many self-hosted runtimes) leave the model's raw
/// output in `content` rather than post-processing it into the `tool_calls`
/// JSON array. This function recognises the common leaked formats so the
/// kernel can dispatch tool calls correctly even from under-configured servers.
///
/// Formats recognised:
///
/// 1. JSON inside `<tool_call>` tags (Qwen3.5 native template):
///    `<tool_call>{"name": "sql_exec", "arguments": {...}}</tool_call>`
///
/// 2. XML parameter blocks inside `<tool_call>` tags (Qwen3 XML function format):
///    `<tool_call><function=sql_exec><parameter=query>...</parameter></function></tool_call>`
///
/// 3. Gemma-4 special-token format:
///    `<|tool_call>call:sql_exec{query:<|"|>SELECT 1<|"|>}<tool_call|>`
///    Gemma may also embed standard JSON (double-brace): fast-path via JSON parse.
///
/// 4. Python-style named kwargs in fenced python/tool_code blocks:
///    ` ```python\nsql_exec(query="SELECT 1", params=[])\n``` `
///
/// 5. Bare or fenced JSON object (fallback):
///    ` ```json\n{"name": "sql_exec", "arguments": {...}}\n``` `
///
/// Conservative: only fires when tool names match a sent tool and the parsed
/// object has an `arguments`/`parameters` object key. Returns empty vec on
/// no match so the caller leaves `content` untouched.
fn extract_tool_calls_from_content(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    let mut results = Vec::new();

    let try_parse = |raw: &str| -> Option<ToolCall> {
        let v: Value = serde_json::from_str(raw.trim()).ok()?;
        let name = v.get("name").and_then(|n| n.as_str())?.to_string();
        if !tool_names.contains(&name.as_str()) {
            return None;
        }
        let args = v
            .get("arguments")
            .or_else(|| v.get("parameters"))
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));
        if !args.is_object() {
            return None;
        }
        let id = format!("call_{}", llama_uuid_hex());
        Some(ToolCall { id, name, args })
    };

    // Format 1 + 2: <tool_call>...</tool_call> blocks.
    {
        let mut search = content;
        while let Some(start) = search.find("<tool_call>") {
            let after_open = &search[start + "<tool_call>".len()..];
            if let Some(end) = after_open.find("</tool_call>") {
                let inner = after_open[..end].trim();
                if let Some(tc) = try_parse(inner) {
                    results.push(tc);
                } else if let Some(tc) = parse_llama_xml_function_block(inner, tool_names) {
                    results.push(tc);
                }
                search = &after_open[end + "</tool_call>".len()..];
            } else {
                break;
            }
        }
    }

    // Format 3 (Gemma-4): <|tool_call>call:NAME{...}<tool_call|> blocks.
    // Try this before fenced JSON, as Gemma uses special token delimiters that
    // will not match the generic JSON patterns.
    if results.is_empty() && content.contains("<|tool_call>") {
        results.extend(parse_gemma_tool_call(content, tool_names));
    }

    // Format 4 (python_tools / xml_tools): Python named-kwarg calls in fenced
    // code blocks or as bare lines. SmolLM3 and similar models emit calls like
    // `sql_exec(query="SELECT 1", params=[])` inside python/tool_code blocks.
    if results.is_empty() {
        results.extend(parse_python_tool_calls(content, tool_names));
    }

    // Format 5: fenced or bare JSON (only if none of the above matched).
    if results.is_empty() {
        let candidates: Vec<&str> = {
            let mut c = Vec::new();
            let mut s = content;
            while let Some(open) = s.find("```") {
                let after = &s[open + 3..];
                let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
                let body = &after[body_start..];
                if let Some(close) = body.find("```") {
                    c.push(body[..close].trim());
                    s = &body[close + 3..];
                } else {
                    break;
                }
            }
            c.push(content.trim());
            c
        };
        for raw in candidates {
            if let Some(tc) = try_parse(raw) {
                results.push(tc);
                break;
            }
        }
    }

    results
}

/// Parse `<function=name><parameter=p>value</parameter>...</function>` blocks.
fn parse_llama_xml_function_block(inner: &str, tool_names: &[&str]) -> Option<ToolCall> {
    let func_start = inner.find("<function=")?;
    let after_func = &inner[func_start + "<function=".len()..];
    let name_end = after_func.find('>')?;
    let name = after_func[..name_end].trim().to_string();
    if !tool_names.contains(&name.as_str()) {
        return None;
    }
    let body = &after_func[name_end + 1..];
    let body = if let Some(e) = body.rfind("</function>") {
        &body[..e]
    } else {
        body
    };

    let mut args = serde_json::Map::new();
    let mut search = body;
    while let Some(p_start) = search.find("<parameter=") {
        let after_p = &search[p_start + "<parameter=".len()..];
        let key_end = after_p.find('>')?;
        let key = after_p[..key_end].trim().to_string();
        let val_str = &after_p[key_end + 1..];
        let val_end = val_str.find("</parameter>").unwrap_or(val_str.len());
        let val = val_str[..val_end].trim();
        let json_val: Value =
            serde_json::from_str(val).unwrap_or_else(|_| Value::String(val.to_string()));
        args.insert(key, json_val);
        search = &val_str[val_end + "</parameter>".len().min(val_str.len())..];
    }

    let id = format!("call_{}", llama_uuid_hex());
    Some(ToolCall {
        id,
        name,
        args: Value::Object(args),
    })
}

/// Parse Python-style named-kwarg tool calls from content (llama-cpp copy).
///
/// See the OpenAI provider for the full doc. This is a verbatim copy to keep
/// both providers self-contained (no shared dep between plugin crates).
fn parse_python_tool_calls(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    let mut results = Vec::new();
    let mut candidates: Vec<&str> = Vec::new();
    {
        let mut s = content;
        while let Some(open) = s.find("```") {
            let after = &s[open + 3..];
            let lang_end = after.find('\n').unwrap_or(after.len());
            let lang = after[..lang_end].trim().to_lowercase();
            let body_start = lang_end + 1;
            if body_start >= after.len() {
                break;
            }
            let body = &after[body_start..];
            if let Some(close) = body.find("```") {
                if matches!(lang.as_str(), "python" | "tool_code" | "tool_call" | "") {
                    for line in body[..close].lines() {
                        let t = line.trim();
                        if !t.is_empty() {
                            candidates.push(t);
                        }
                    }
                }
                s = &body[close + 3..];
            } else {
                break;
            }
        }
    }
    for line in content.lines() {
        let t = line.trim();
        if !t.is_empty() {
            candidates.push(t);
        }
    }

    for line in candidates {
        let Some(paren) = line.find('(') else {
            continue;
        };
        let name = line[..paren].trim();
        if !tool_names.contains(&name) {
            continue;
        }
        let Some(close) = line.rfind(')') else {
            continue;
        };
        if close <= paren {
            continue;
        }
        let args_str = &line[paren + 1..close];
        if !args_str.contains('=') {
            continue;
        }

        let mut args = serde_json::Map::new();
        let mut depth: i32 = 0;
        let mut token_start = 0;
        let bytes = args_str.as_bytes();
        let mut i = 0;
        let mut tokens: Vec<&str> = Vec::new();
        while i < bytes.len() {
            match bytes[i] {
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth -= 1,
                b',' if depth == 0 => {
                    tokens.push(&args_str[token_start..i]);
                    token_start = i + 1;
                }
                _ => {}
            }
            i += 1;
        }
        tokens.push(&args_str[token_start..]);

        for token in tokens {
            let token = token.trim();
            let Some(eq) = token.find('=') else { continue };
            let key = token[..eq].trim().to_string();
            if key.is_empty() || key.contains(' ') {
                continue;
            }
            let raw_val = token[eq + 1..].trim();
            let json_val: Value = if let Ok(v) = serde_json::from_str(raw_val) {
                v
            } else if (raw_val.starts_with('\'') && raw_val.ends_with('\''))
                || (raw_val.starts_with('"') && raw_val.ends_with('"'))
            {
                let inner = &raw_val[1..raw_val.len() - 1];
                Value::String(inner.to_string())
            } else {
                Value::String(raw_val.to_string())
            };
            args.insert(key, json_val);
        }

        if args.is_empty() {
            continue;
        }

        let id = format!("call_{}", llama_uuid_hex());
        results.push(ToolCall {
            id,
            name: name.to_string(),
            args: Value::Object(args),
        });
        break;
    }

    results
}

/// Parse Gemma-4's native tool-call format:
/// `<|tool_call>call:NAME{key:VALUE,...}<tool_call|>`
///
/// Gemma-4 uses `<|"|>` as a special-token quote and emits args in a
/// brace-delimited comma-separated key:value format. Example:
///   `<|tool_call>call:sql_exec{query:<|"|>SELECT 1<|"|>,params:[]}<tool_call|>`
fn parse_gemma_tool_call(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    const OPEN: &str = "<|tool_call>call:";
    const CLOSE: &str = "<tool_call|>";
    const QMARK: &str = "<|\"|>";

    let mut results = Vec::new();
    let mut search = content;
    while let Some(start) = search.find(OPEN) {
        let after = &search[start + OPEN.len()..];
        // Find the function name (up to '{').
        let Some(brace) = after.find('{') else { break };
        let name = after[..brace].trim().to_string();
        if !tool_names.contains(&name.as_str()) {
            search = &after[brace..];
            continue;
        }
        let rest = &after[brace..];
        // Find closing CLOSE tag; everything between '{' and the end of CLOSE
        // is the args blob.
        let end = rest.find(CLOSE).unwrap_or(rest.len());
        let args_blob = rest[..end].trim();
        // Strip outer braces.
        let args_blob = args_blob
            .strip_prefix('{')
            .and_then(|s| s.strip_suffix('}'))
            .unwrap_or(args_blob);

        // Fast path: if the stripped blob is a valid JSON object (Gemma
        // sometimes emits standard JSON inside its special-token format, e.g.
        // `{{"content":"v","path":"p"}}` which yields `{"content":"v","path":"p"}`
        // after one layer of brace stripping), parse it directly.
        let args = if let Ok(Value::Object(map)) = serde_json::from_str::<Value>(args_blob.trim()) {
            map
        } else {
            // Slow path: parse Gemma's key:value format.
            // Values may be:
            //   - quoted strings: <|"|>text<|"|>
            //   - arrays: [<|"|>v1<|"|>,<|"|>v2<|"|>]
            //   - bare numbers / true / false / null
            let mut map = serde_json::Map::new();
            let mut rem = args_blob;
            while !rem.is_empty() {
                rem = rem.trim_start_matches([',', ' ', '\n', '\t']);
                if rem.is_empty() {
                    break;
                }
                // Key ends at ':'.
                let Some(colon) = rem.find(':') else { break };
                let key = rem[..colon].trim().to_string();
                rem = &rem[colon + 1..];

                let val_json: Value;
                if rem.starts_with(QMARK) {
                    // Quoted string: find matching close QMARK.
                    let inner = &rem[QMARK.len()..];
                    let close = inner.find(QMARK).unwrap_or(inner.len());
                    let s = &inner[..close];
                    val_json = json!(s);
                    rem = &inner[close + QMARK.len()..];
                } else if rem.starts_with('[') {
                    // Array: collect items up to ']'.
                    let Some(arr_end) = rem.find(']') else { break };
                    let arr_str = &rem[1..arr_end];
                    let mut items = Vec::new();
                    let mut a = arr_str;
                    while !a.is_empty() {
                        a = a.trim_start_matches([',', ' ']);
                        if a.is_empty() {
                            break;
                        }
                        if a.starts_with(QMARK) {
                            let inner = &a[QMARK.len()..];
                            let close = inner.find(QMARK).unwrap_or(inner.len());
                            items.push(json!(inner[..close].to_string()));
                            a = &inner[close + QMARK.len()..];
                        } else {
                            // Bare value.
                            let end = a.find([',', ']']).unwrap_or(a.len());
                            let v: Value =
                                serde_json::from_str(a[..end].trim()).unwrap_or(Value::Null);
                            items.push(v);
                            a = &a[end..];
                        }
                    }
                    val_json = Value::Array(items);
                    rem = &rem[arr_end + 1..];
                } else {
                    // Bare value (number, bool, null).
                    let end = rem.find(',').unwrap_or(rem.len());
                    val_json = serde_json::from_str(rem[..end].trim())
                        .unwrap_or_else(|_| json!(rem[..end].trim()));
                    rem = &rem[end..];
                }
                map.insert(key, val_json);
            }
            map
        };

        let id = format!("call_{}", llama_uuid_hex());
        results.push(ToolCall {
            id,
            name,
            args: Value::Object(args),
        });

        if end < rest.len() {
            search = &rest[end + CLOSE.len()..];
        } else {
            break;
        }
    }
    results
}

fn llama_uuid_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    std::thread_local! {
        static CTR: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
    }
    let ctr = CTR.with(|c| {
        let v = c.get();
        c.set(v.wrapping_add(1));
        v
    });
    format!("{nanos:08x}{ctr:04x}")
}

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
                    // Use empty string instead of null for content when absent.
                    // llama-cpp-python's pydantic validator rejects content:null
                    // in assistant messages that carry tool_calls, which causes
                    // HTTP 500 on the follow-up turn after content-extraction
                    // (Fix B6) synthesises tool_calls with content=None.
                    let content_val = m
                        .content
                        .as_deref()
                        .map(|s| json!(s))
                        .unwrap_or_else(|| json!(""));
                    messages.push(json!({
                        "role": "assistant",
                        "content": content_val,
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
                // B16: when PG_SYNAPSE_LOG_RAW_LLM=1, log every parsed tool call
                // so the raw args are observable when a deserialization error follows.
                if std::env::var("PG_SYNAPSE_LOG_RAW_LLM").as_deref() == Ok("1") {
                    let log_line = format!("RAW_TOOL_CALL tool={} args={}\n", name, args_str);
                    let _ = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open("/tmp/pg_synapse_raw_llm.log")
                        .and_then(|mut f| {
                            use std::io::Write;
                            f.write_all(log_line.as_bytes())
                        });
                }
                tool_calls.push(ToolCall { id, name, args });
            }
        }

        // Fallback (Fix B6): llama-cpp-python with default chat_format leaves the
        // model's raw output in `content` rather than post-processing into
        // `tool_calls`. Extract structured calls when the server did not.
        if tool_calls.is_empty() && !req.tools.is_empty() {
            if let Some(ref text) = content {
                if !text.is_empty() {
                    // Debug mode: when PG_SYNAPSE_LOG_RAW_LLM=1 is set, append
                    // the raw content to /tmp/pg_synapse_raw_llm.log so the
                    // exact format each model emits is observable.
                    if std::env::var("PG_SYNAPSE_LOG_RAW_LLM").as_deref() == Ok("1") {
                        let snippet: String = text.chars().take(1200).collect();
                        let line = format!(
                            "RAW_LLM_NO_TOOLCALL model={} tools={} content={}\n",
                            self.model,
                            req.tools
                                .iter()
                                .map(|t| t.name.as_str())
                                .collect::<Vec<_>>()
                                .join(","),
                            snippet
                        );
                        let _ = std::fs::OpenOptions::new()
                            .create(true)
                            .append(true)
                            .open("/tmp/pg_synapse_raw_llm.log")
                            .and_then(|mut f| {
                                use std::io::Write;
                                f.write_all(line.as_bytes())
                            });
                        tracing::warn!(
                            target: "pg_synapse_llama_cpp",
                            "RAW_LLM_NO_TOOLCALL: {}",
                            snippet
                        );
                    }
                    let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
                    let extracted = extract_tool_calls_from_content(text, &names);
                    if !extracted.is_empty() {
                        tracing::debug!(
                            target: "pg_synapse_llama_cpp",
                            count = extracted.len(),
                            "synthesised tool_calls from content (llama-cpp server did not populate tool_calls)"
                        );
                        tool_calls = extracted;
                    }
                }
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
