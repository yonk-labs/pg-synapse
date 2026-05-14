//! LLM request, response, and streaming-chunk types.

use serde::{Deserialize, Serialize};
use std::ops::Add;

use super::message::Message;
use super::tool::ToolSchema;

/// One LLM completion request.
///
/// Provider-specific extras (top_p, presence_penalty, ...) live under
/// [`CompletionRequest::params`] so the kernel doesn't need to grow per the
/// union of every provider's knobs.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionRequest {
    /// Conversation history in chronological order.
    pub messages: Vec<Message>,
    /// Tool schemas the model may call.
    pub tools: Vec<ToolDefinition>,
    /// Model name (overrides the profile's default when set).
    pub model: Option<String>,
    /// Sampling temperature.
    pub temperature: Option<f32>,
    /// Maximum tokens to generate.
    pub max_tokens: Option<u32>,
    /// Provider-specific extras passed through verbatim.
    pub params: serde_json::Value,
}

/// A tool advertised to the model. Pairs a name + description with a JSON Schema.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ToolDefinition {
    /// Tool name (must match a tool in the active registry).
    pub name: String,
    /// Short natural-language description fed to the model.
    pub description: String,
    /// JSON Schema for the tool's input.
    pub schema: ToolSchema,
}

impl ToolDefinition {
    /// Render this tool as an OpenAI Chat Completions "function" object.
    ///
    /// Returns a JSON value shaped as
    /// `{"name": ..., "description": ..., "parameters": <json-schema>}`,
    /// suitable for embedding under `tools[i].function` in an OpenAI request.
    ///
    /// Provider plugins targeting OpenAI-compatible endpoints (vLLM,
    /// llama-cpp-server, LM Studio, the OpenAI Ollama shim) call this to
    /// avoid duplicating the conversion logic.
    pub fn to_openai_function(&self) -> serde_json::Value {
        let parameters = serde_json::to_value(self.schema.as_root_schema())
            .unwrap_or_else(|_| serde_json::json!({"type": "object"}));
        serde_json::json!({
            "name": self.name,
            "description": self.description,
            "parameters": parameters,
        })
    }
}

/// Synchronous completion response.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionResponse {
    /// Text content, if any.
    pub content: Option<String>,
    /// Tool calls the model issued. Empty when the model produced only text.
    pub tool_calls: Vec<ToolCall>,
    /// Provider-reported finish reason ("stop", "length", "tool_calls", ...).
    pub finish_reason: String,
    /// Token + cost accounting.
    pub usage: Usage,
}

/// One delta from a streamed completion. Empty fields are normal between
/// meaningful deltas; consumers should accumulate until `finish_reason` is set.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct CompletionChunk {
    /// Incremental text, if any.
    pub delta_content: Option<String>,
    /// Incremental tool-call deltas. Most providers send these one-at-a-time.
    pub delta_tool_calls: Vec<ToolCall>,
    /// Set on the final chunk only.
    pub finish_reason: Option<String>,
}

/// One tool call issued by the model.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ToolCall {
    /// Provider-issued call ID. Echoed back when sending the tool result.
    pub id: String,
    /// Tool name (must be present in the active registry).
    pub name: String,
    /// JSON arguments, conforming to the tool's schema.
    pub args: serde_json::Value,
}

/// Token + cost accounting for one LLM call. Accumulates with `+`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq)]
pub struct Usage {
    /// Tokens in the prompt.
    pub tokens_in: u32,
    /// Tokens produced.
    pub tokens_out: u32,
    /// Cost in USD; `None` if the provider has no pricing data registered.
    pub cost_usd: Option<f64>,
}

impl Add for Usage {
    type Output = Usage;
    fn add(self, rhs: Usage) -> Usage {
        let cost_usd = match (self.cost_usd, rhs.cost_usd) {
            (Some(a), Some(b)) => Some(a + b),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        Usage {
            tokens_in: self.tokens_in.saturating_add(rhs.tokens_in),
            tokens_out: self.tokens_out.saturating_add(rhs.tokens_out),
            cost_usd,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completion_request_default_roundtrips() {
        let req = CompletionRequest::default();
        let s = serde_json::to_string(&req).unwrap();
        let back: CompletionRequest = serde_json::from_str(&s).unwrap();
        assert_eq!(req, back);
    }

    #[test]
    fn completion_response_default_roundtrips() {
        let resp = CompletionResponse::default();
        let s = serde_json::to_string(&resp).unwrap();
        let back: CompletionResponse = serde_json::from_str(&s).unwrap();
        assert_eq!(resp, back);
    }

    #[test]
    fn completion_chunk_roundtrips() {
        let c = CompletionChunk {
            delta_content: Some("ab".into()),
            delta_tool_calls: vec![],
            finish_reason: Some("stop".into()),
        };
        let s = serde_json::to_string(&c).unwrap();
        let back: CompletionChunk = serde_json::from_str(&s).unwrap();
        assert_eq!(c, back);
    }

    #[test]
    fn usage_add_sums_tokens_and_costs() {
        let a = Usage {
            tokens_in: 10,
            tokens_out: 20,
            cost_usd: Some(0.01),
        };
        let b = Usage {
            tokens_in: 5,
            tokens_out: 7,
            cost_usd: Some(0.02),
        };
        let c = a + b;
        assert_eq!(c.tokens_in, 15);
        assert_eq!(c.tokens_out, 27);
        assert_eq!(c.cost_usd, Some(0.03));
    }

    #[test]
    fn usage_add_handles_partial_costs() {
        let a = Usage {
            tokens_in: 1,
            tokens_out: 1,
            cost_usd: Some(0.5),
        };
        let b = Usage {
            tokens_in: 1,
            tokens_out: 1,
            cost_usd: None,
        };
        assert_eq!((a + b).cost_usd, Some(0.5));
        assert_eq!((Usage::default() + Usage::default()).cost_usd, None);
    }

    #[test]
    fn tool_definition_to_openai_function_shape() {
        use super::super::tool::ToolSchema;
        let schema = ToolSchema::from_json_value(serde_json::json!({
            "title": "EchoInput",
            "type": "object",
            "properties": { "message": { "type": "string" } },
            "required": ["message"]
        }))
        .unwrap();
        let td = ToolDefinition {
            name: "echo".into(),
            description: "Echo a string back.".into(),
            schema,
        };
        let v = td.to_openai_function();
        assert_eq!(v["name"], "echo");
        assert_eq!(v["description"], "Echo a string back.");
        assert_eq!(v["parameters"]["title"], "EchoInput");
    }

    #[test]
    fn tool_call_debug_format_contains_name() {
        let t = ToolCall {
            id: "1".into(),
            name: "weather".into(),
            args: serde_json::json!({}),
        };
        let s = format!("{:?}", t);
        assert!(s.contains("weather"));
    }
}
