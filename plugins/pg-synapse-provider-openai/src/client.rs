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

/// Try to extract tool calls from a content string when the serving stack did
/// not populate `tool_calls` in the response (common with llama-cpp-python and
/// other self-hosted runtimes that do not post-process the model output).
///
/// Formats recognised:
///
/// Format 1 - JSON inside `<tool_call>` tags (Qwen3.5 XML template):
/// ```text
/// <tool_call>
/// {"name": "sql_exec", "arguments": {"query": "...", "params": [...]}}
/// </tool_call>
/// ```
///
/// Format 2 - Gemma-4 special-token format:
/// ```text
/// <|tool_call>call:sql_exec{query:<|"|>SELECT 1<|"|>,params:[]}<tool_call|>
/// ```
/// Gemma may also emit standard JSON inside its special tokens (double-brace):
/// `{{"content":"v","path":"p"}}` is parsed via a JSON fast-path.
///
/// Format 3 - Python/tool_code kwargs in fenced blocks (xml_tools / python_tools):
/// ```python
/// sql_exec(query="INSERT INTO t VALUES ($1)", params=["ok"])
/// ```
/// Only named keyword arguments are extracted; positional-only calls are skipped
/// to avoid false positives. Only matches when the function name is a sent tool.
///
/// Format 4 - fenced JSON object (bare, or inside triple-backtick block):
/// ```json
/// {"name": "sql_exec", "arguments": {"query": "..."}}
/// ```
///
/// Conservative matching rules (to avoid false positives on normal prose):
/// * Only fires when the request actually sent `tools` (enforced by the caller).
/// * The parsed object must have a `name` key whose value matches one of the
///   sent tool names.
/// * The parsed object must have an `arguments` or `parameters` key that is
///   a JSON object.
/// * If none of the above match, the function returns an empty vec so the
///   caller leaves `content` untouched.
pub fn extract_tool_calls_from_content(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    let mut results = Vec::new();

    // Helper: try to parse a JSON string as a tool-call object.
    let try_parse = |raw: &str| -> Option<ToolCall> {
        let v: Value = serde_json::from_str(raw.trim()).ok()?;
        let name = v.get("name").and_then(|n| n.as_str())?.to_string();
        if !tool_names.contains(&name.as_str()) {
            return None;
        }
        // Accept both "arguments" and "parameters" as the args key.
        let args = v
            .get("arguments")
            .or_else(|| v.get("parameters"))
            .cloned()
            .unwrap_or(Value::Object(serde_json::Map::new()));
        if !args.is_object() {
            return None;
        }
        let id = format!("call_{}", uuid_v4_hex());
        Some(ToolCall { id, name, args })
    };

    // Format 1: <tool_call>...</tool_call> blocks.
    // The content between the tags may be JSON, or it may be XML-parameter
    // format. We handle both sub-variants.
    {
        let mut search = content;
        while let Some(start) = search.find("<tool_call>") {
            let after_open = &search[start + "<tool_call>".len()..];
            if let Some(end) = after_open.find("</tool_call>") {
                let inner = after_open[..end].trim();
                // Sub-variant A: JSON object directly inside the tags.
                if let Some(tc) = try_parse(inner) {
                    results.push(tc);
                }
                // Sub-variant B: <function=name><parameter=p>v</parameter></function>
                // Only attempt if JSON parsing failed (inner not valid JSON).
                else if let Some(tc) = parse_xml_function_block(inner, tool_names) {
                    results.push(tc);
                }
                search = &after_open[end + "</tool_call>".len()..];
            } else {
                break;
            }
        }
    }

    // Format 2 (Gemma-4): <|tool_call>call:NAME{...}<tool_call|> blocks.
    // Gemma-4 uses special token delimiters; try before fenced JSON.
    if results.is_empty() && content.contains("<|tool_call>") {
        results.extend(parse_gemma_tool_call_openai(content, tool_names));
    }

    // Format 3 (python_tools / xml_tools): Python-style kwargs in fenced blocks.
    // SmolLM3 and similar models may emit calls like:
    //   ```python\nsql_exec(query="SELECT 1", params=[])\n```
    // Only named kwargs are matched; positional-only calls are skipped.
    if results.is_empty() {
        results.extend(parse_python_tool_calls(content, tool_names));
    }

    // Format 4: bare JSON object in a fenced code block or inline.
    // Only attempt this if nothing matched above, to keep false-positive rate
    // low (a model might output valid JSON for other reasons).
    if results.is_empty() {
        // Try triple-backtick fenced blocks first, then bare.
        let candidates: Vec<&str> = {
            let mut c = Vec::new();
            let mut s = content;
            while let Some(open) = s.find("```") {
                let after = &s[open + 3..];
                // Skip optional language tag on first line.
                let body_start = after.find('\n').map(|i| i + 1).unwrap_or(0);
                let body = &after[body_start..];
                if let Some(close) = body.find("```") {
                    c.push(body[..close].trim());
                    s = &body[close + 3..];
                } else {
                    break;
                }
            }
            // Also try the full content stripped of surrounding whitespace.
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
fn parse_xml_function_block(inner: &str, tool_names: &[&str]) -> Option<ToolCall> {
    // Expect: <function=NAME> ... </function>
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

    // Extract <parameter=KEY>VALUE</parameter> pairs.
    let mut args = serde_json::Map::new();
    let mut search = body;
    while let Some(p_start) = search.find("<parameter=") {
        let after_p = &search[p_start + "<parameter=".len()..];
        let key_end = after_p.find('>')?;
        let key = after_p[..key_end].trim().to_string();
        let val_str = &after_p[key_end + 1..];
        let val_end = val_str.find("</parameter>").unwrap_or(val_str.len());
        let val = val_str[..val_end].trim();
        // Try to parse value as JSON; fall back to string.
        let json_val: Value =
            serde_json::from_str(val).unwrap_or_else(|_| Value::String(val.to_string()));
        args.insert(key, json_val);
        search = &val_str[val_end + "</parameter>".len().min(val_str.len())..];
    }

    let id = format!("call_{}", uuid_v4_hex());
    Some(ToolCall {
        id,
        name,
        args: Value::Object(args),
    })
}

/// Parse Python-style named-kwarg tool calls from content.
///
/// Matches calls like `tool_name(key="val", n=5, flag=true)` that appear
/// as bare lines or inside fenced code blocks tagged `python`, `tool_code`,
/// or `tool_call`. Only named keyword arguments are extracted; positional-only
/// calls are skipped to avoid false positives (we cannot map positional args to
/// JSON field names without per-tool schemas).
///
/// Conservative: only matches when the parsed name is in `tool_names` AND
/// at least one named kwarg is present. Pure positional calls (`func(a, b)`)
/// return nothing.
fn parse_python_tool_calls(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    let mut results = Vec::new();

    // Collect candidate lines: from fenced python/tool_code/tool_call blocks
    // or from every non-empty line in the whole content.
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
    // Also scan every line of the full content for bare calls.
    for line in content.lines() {
        let t = line.trim();
        if !t.is_empty() {
            candidates.push(t);
        }
    }

    for line in candidates {
        // Line must start with a tool name followed immediately by '('.
        let Some(paren) = line.find('(') else {
            continue;
        };
        let name = line[..paren].trim();
        if !tool_names.contains(&name) {
            continue;
        }
        // Find the matching close paren (simple: last ')' on the line).
        let Some(close) = line.rfind(')') else {
            continue;
        };
        if close <= paren {
            continue;
        }
        let args_str = &line[paren + 1..close];

        // Only accept if there is at least one `key=` pattern (named kwarg).
        // This guards against positional-only calls like `func("a", "b")`.
        if !args_str.contains('=') {
            continue;
        }

        // Parse named kwargs. Simple tokenizer: split on top-level commas,
        // then split each token on the first '=' to get key and value.
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
            // Try JSON parse; if the raw_val is a Python string (single or
            // double quoted), normalize to double-quoted JSON.
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

        let id = format!("call_{}", uuid_v4_hex());
        results.push(ToolCall {
            id,
            name: name.to_string(),
            args: Value::Object(args),
        });
        // Stop after first match on this line (one call per line).
        break;
    }

    results
}

/// Parse Gemma-4's native tool-call format (OpenAI provider copy).
/// `<|tool_call>call:NAME{key:VALUE,...}<tool_call|>`
fn parse_gemma_tool_call_openai(content: &str, tool_names: &[&str]) -> Vec<ToolCall> {
    const OPEN: &str = "<|tool_call>call:";
    const CLOSE: &str = "<tool_call|>";
    const QMARK: &str = "<|\"|>";

    let mut results = Vec::new();
    let mut search = content;
    while let Some(start) = search.find(OPEN) {
        let after = &search[start + OPEN.len()..];
        let Some(brace) = after.find('{') else { break };
        let name = after[..brace].trim().to_string();
        if !tool_names.contains(&name.as_str()) {
            search = &after[brace..];
            continue;
        }
        let rest = &after[brace..];
        let end = rest.find(CLOSE).unwrap_or(rest.len());
        let args_blob = rest[..end].trim();
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
            let mut map = serde_json::Map::new();
            let mut rem = args_blob;
            while !rem.is_empty() {
                rem = rem.trim_start_matches([',', ' ', '\n', '\t']);
                if rem.is_empty() {
                    break;
                }
                let Some(colon) = rem.find(':') else { break };
                let key = rem[..colon].trim().to_string();
                rem = &rem[colon + 1..];
                let val_json: Value;
                if rem.starts_with(QMARK) {
                    let inner = &rem[QMARK.len()..];
                    let close = inner.find(QMARK).unwrap_or(inner.len());
                    val_json = json!(inner[..close].to_string());
                    rem = &inner[close + QMARK.len()..];
                } else if rem.starts_with('[') {
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
                    let end = rem.find(',').unwrap_or(rem.len());
                    val_json = serde_json::from_str(rem[..end].trim())
                        .unwrap_or_else(|_| json!(rem[..end].trim()));
                    rem = &rem[end..];
                }
                map.insert(key, val_json);
            }
            map
        };

        let id = format!("call_{}", uuid_v4_hex());
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

fn uuid_v4_hex() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    // Mix time nanos with a thread-local counter for uniqueness within a process.
    // This is not cryptographically random, but tool-call IDs only need to be
    // unique within a single completion response.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.subsec_nanos())
        .unwrap_or(0);
    // Thread-local counter.
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

        // Fallback: some self-hosted runtimes (llama-cpp-python with default
        // chat_format, Ollama, etc.) never populate `tool_calls` in the wire
        // response even when the model actually emitted a structured tool call
        // inside `content`. When the request sent tools and the response
        // `tool_calls` array is empty, attempt to extract calls from `content`.
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
                            target: "pg_synapse_openai",
                            "RAW_LLM_NO_TOOLCALL: {}",
                            snippet
                        );
                    }
                    let names: Vec<&str> = req.tools.iter().map(|t| t.name.as_str()).collect();
                    let extracted = extract_tool_calls_from_content(text, &names);
                    if !extracted.is_empty() {
                        tracing::debug!(
                            target: "pg_synapse_openai",
                            count = extracted.len(),
                            "synthesised tool_calls from content (server did not populate tool_calls)"
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

    // B6: tool-call extraction from content -- fenced JSON format.
    #[test]
    fn extract_tool_calls_fenced_json() {
        let content = "Sure!\n\n```json\n{\"name\": \"sql_exec\", \"arguments\": {\"query\": \"INSERT INTO t(a) VALUES ($1)\", \"params\": [\"hi\"]}}\n```";
        let names = &["sql_exec", "sql_query"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "sql_exec");
        assert_eq!(calls[0].args["query"], "INSERT INTO t(a) VALUES ($1)");
        assert_eq!(calls[0].args["params"][0], "hi");
    }

    // B6: tool-call extraction from content -- <tool_call> tag format with JSON inside.
    #[test]
    fn extract_tool_calls_tag_json() {
        let content = "I will call the function.\n<tool_call>\n{\"name\": \"sql_exec\", \"arguments\": {\"query\": \"SELECT 1\"}}\n</tool_call>";
        let names = &["sql_exec"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "sql_exec");
        assert_eq!(calls[0].args["query"], "SELECT 1");
        assert!(!calls[0].id.is_empty());
    }

    // B6: tool-call extraction -- <tool_call><function=name><parameter=p>v</parameter></function></tool_call> format.
    #[test]
    fn extract_tool_calls_tag_xml_params() {
        let content = "<tool_call>\n<function=sql_exec>\n<parameter=query>\nSELECT 42\n</parameter>\n<parameter=params>\n[]\n</parameter>\n</function>\n</tool_call>";
        let names = &["sql_exec"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "sql_exec");
        assert_eq!(calls[0].args["query"], "SELECT 42");
        // params parsed as JSON array
        assert!(calls[0].args["params"].is_array());
    }

    // B6: tool-call extraction -- plain prose with tools sent must NOT synthesise
    // a tool call (no false positives).
    #[test]
    fn extract_tool_calls_negative_plain_prose() {
        let content =
            "I cannot insert rows directly. Please provide the database connection details.";
        let names = &["sql_exec", "sql_query"];
        let calls = extract_tool_calls_from_content(content, names);
        assert!(
            calls.is_empty(),
            "plain prose must not produce synthesised tool calls"
        );
    }

    // B13: Gemma double-brace JSON inside special-token format.
    // Real payload captured from gemma-4-E2B-it f1_find scenario:
    // <|tool_call>call:write_file{{"content":"b.txt:42","path":"found.txt"}}<tool_call|>
    #[test]
    fn extract_tool_calls_gemma_double_brace_json() {
        let content = r#"<|tool_call>call:write_file{{"content":"b.txt:42","path":"bench/found.txt"}}<tool_call|>"#;
        let names = &["write_file", "read_file", "grep"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "write_file");
        assert_eq!(calls[0].args["content"], "b.txt:42");
        assert_eq!(calls[0].args["path"], "bench/found.txt");
    }

    // B13: Gemma standard key:<|"|>value<|"|> format still works after refactor.
    // Real payload captured from gemma-4-E2B-it f1_find scenario:
    // <|tool_call>call:grep{path:<|"|>data<|"|>,pattern:<|"|>TOKEN<|"|>}<tool_call|>
    #[test]
    fn extract_tool_calls_gemma_standard_qmark_format() {
        let content = "<|tool_call>call:grep{path:<|\"|>bench/data<|\"|>,pattern:<|\"|>THE_SECRET_TOKEN<|\"|>}<tool_call|>";
        let names = &["write_file", "read_file", "grep"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "grep");
        assert_eq!(calls[0].args["path"], "bench/data");
        assert_eq!(calls[0].args["pattern"], "THE_SECRET_TOKEN");
    }

    // B13: python_tools named-kwarg format in a fenced python block.
    // Models like SmolLM3 (SQL scenarios) emit:
    //   ```python\nsql_exec(query="SELECT 1", params=[])\n```
    #[test]
    fn extract_tool_calls_python_kwargs_fenced() {
        let content =
            "Here is the call:\n\n```python\nsql_exec(query=\"SELECT 1\", params=[])\n```";
        let names = &["sql_exec", "sql_query"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "sql_exec");
        assert_eq!(calls[0].args["query"], "SELECT 1");
        assert!(calls[0].args["params"].is_array());
    }

    // B13: python_tools bare line format (no code fence).
    #[test]
    fn extract_tool_calls_python_kwargs_bare_line() {
        let content =
            "I will call the tool now.\nwrite_file(path=\"out.txt\", content=\"hello\")\nDone.";
        let names = &["write_file", "read_file"];
        let calls = extract_tool_calls_from_content(content, names);
        assert_eq!(calls.len(), 1, "expected one extracted tool call");
        assert_eq!(calls[0].name, "write_file");
        assert_eq!(calls[0].args["path"], "out.txt");
        assert_eq!(calls[0].args["content"], "hello");
    }

    // B13: python_tools positional-only (no kwargs) must NOT synthesise a call.
    #[test]
    fn extract_tool_calls_python_positional_only_no_false_positive() {
        let content = "```python\nsql_exec(\"SELECT 1\", [])\n```";
        let names = &["sql_exec"];
        let calls = extract_tool_calls_from_content(content, names);
        assert!(
            calls.is_empty(),
            "positional-only Python call must not produce a synthesised tool call"
        );
    }

    // B13: prose containing a tool name in parentheses must NOT synthesise a call.
    // e.g. "call write_file(path=...)" where write_file is mentioned but context is prose.
    #[test]
    fn extract_tool_calls_negative_prose_with_tool_name() {
        let content =
            "You can use write_file to save results. The write_file function takes a path.";
        let names = &["write_file", "read_file"];
        let calls = extract_tool_calls_from_content(content, names);
        assert!(
            calls.is_empty(),
            "prose mentioning tool names without a call must not produce tool calls"
        );
    }

    // B6: live test against gpt-5-mini (skipped without live-tests feature).
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
