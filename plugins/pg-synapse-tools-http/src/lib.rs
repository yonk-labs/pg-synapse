//! HTTP tool plugin for pg_synapse: `http_get`, `http_post`, `http_head`.
//!
//! All three tools are derived via `#[derive(pg_synapse_macros::Tool)]` and
//! share a single lazily-initialized `reqwest::Client` with a 30 s default
//! timeout. The `HttpToolsPlugin` newtype implements `pg_synapse_core::Plugin`
//! and registers all three tools into a host `Registry`.
//!
//! ## Output shape
//!
//! * `http_get` / `http_post`: `{ "status": <u16>, "body": <string> }`
//! * `http_head`: `{ "status": <u16>, "headers": { ... } }`
//!
//! Network or transport failures surface as `ToolError::Execution { name,
//! reason }` with `reason` taken from the underlying `reqwest::Error`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput};
use pg_synapse_macros::Tool as DeriveTool;
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Shared HTTP client used by all three tools.
///
/// Lazily built on first access. A 30 second default timeout is applied so
/// runaway tool calls cannot pin an executor's tokio task indefinitely.
fn http() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds with default config")
    })
}

/// Fetch a URL via HTTP GET. Returns the response body as text.
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_get",
    description = "Fetch a URL via HTTP GET. Returns the response status and body as text."
)]
pub struct HttpGet {
    /// Absolute URL to fetch.
    pub url: String,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpGet {
    /// User-facing handler. The derive macro wraps this with input parsing.
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut req = http().get(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "body": body }),
        ))
    }
}

/// POST a JSON body to a URL. Returns the response status and body as text.
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_post",
    description = "POST a JSON body to a URL. Returns the response status and body as text."
)]
pub struct HttpPost {
    /// Absolute URL to post to.
    pub url: String,
    /// Request body, serialized as JSON. Defaults to `null` when omitted.
    #[serde(default)]
    pub body: Value,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpPost {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut req = http().post(&self.url).json(&self.body);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let body = resp.text().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "body": body }),
        ))
    }
}

/// HEAD request: returns the response status and headers (no body).
#[derive(DeriveTool, JsonSchema, Deserialize, Debug)]
#[tool(
    name = "http_head",
    description = "HEAD request: returns the response status and headers (no body)."
)]
pub struct HttpHead {
    /// Absolute URL to inspect.
    pub url: String,
    /// Optional request headers, sent verbatim.
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

impl HttpHead {
    async fn run(self, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let mut req = http().head(&self.url);
        for (k, v) in &self.headers {
            req = req.header(k, v);
        }
        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: Self::TOOL_NAME.into(),
            reason: e.to_string(),
        })?;
        let status = resp.status().as_u16();
        let headers: BTreeMap<String, String> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| v.to_str().ok().map(|s| (k.to_string(), s.to_string())))
            .collect();
        Ok(ToolOutput::Json(
            serde_json::json!({ "status": status, "headers": headers }),
        ))
    }
}

/// Plugin that registers `http_get`, `http_post`, and `http_head` against a
/// host's [`Registry`].
///
/// Each tool is registered as a single shared `Arc<dyn Tool>`. The fields on
/// the template instances are inert: `Tool::run` re-parses input from JSON on
/// every call, so the template values are never read.
#[derive(Default, Debug)]
pub struct HttpToolsPlugin;

impl Plugin for HttpToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-http"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        let getter = HttpGet {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        let poster = HttpPost {
            url: String::new(),
            body: Value::Null,
            headers: BTreeMap::new(),
        };
        let header = HttpHead {
            url: String::new(),
            headers: BTreeMap::new(),
        };
        registry
            .tools
            .add_arc(HttpGet::TOOL_NAME.to_string(), Arc::new(getter));
        registry
            .tools
            .add_arc(HttpPost::TOOL_NAME.to_string(), Arc::new(poster));
        registry
            .tools
            .add_arc(HttpHead::TOOL_NAME.to_string(), Arc::new(header));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_register_inserts_three_tools() {
        let mut reg = Registry::new();
        HttpToolsPlugin.register(&mut reg);
        let mut names = reg.tools.names();
        names.sort();
        assert_eq!(names, vec!["http_get", "http_head", "http_post"]);
    }

    #[test]
    fn plugin_metadata_present() {
        let p = HttpToolsPlugin;
        assert_eq!(p.name(), "pg-synapse-tools-http");
        assert!(!p.version().is_empty());
    }
}
