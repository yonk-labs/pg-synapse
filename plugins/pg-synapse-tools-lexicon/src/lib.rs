//! pg-lexicon schema-context bridge for `pg_synapse`.
//!
//! Exposes a single agent tool, `get_schema_context`, that gives synapse
//! agents schema-grounded NL2SQL: given a natural-language `question` it POSTs
//! to a running [pg-lexicon] context service and returns the assembled context
//! package (relevant tables, columns, joins, sample values, ...) that the
//! agent can feed into `sql_query` / `sql_exec` to write correct SQL.
//!
//! Unlike the derive-based HTTP tools, this tool carries per-plugin
//! configuration (the pg-lexicon `base_url` and an optional bearer `token`),
//! so it uses a manual [`Tool`] impl mirroring `pg-synapse-tools-sql`: the
//! tool struct holds the config and re-parses a small args struct from the
//! (LLM-supplied) JSON input on every call.
//!
//! ## Wire call
//!
//! `POST {base_url}/v1/context-packages` with JSON body:
//!
//! ```json
//! { "connection": "target_ecommerce", "schema": "ecommerce",
//!   "question": "...", "budget": 4000 }
//! ```
//!
//! When a token is configured, an `Authorization: Bearer {token}` header is
//! sent. The response body is returned verbatim as [`ToolOutput::Json`].
//!
//! ## Output shape
//!
//! * success (HTTP 2xx): `ToolOutput::Json(<pg-lexicon response body>)`.
//! * non-2xx: [`ToolError::Execution`] carrying the status code and body.
//!
//! [pg-lexicon]: https://github.com/yonk-labs/pg-lexicon

#![forbid(unsafe_code)]
#![warn(missing_docs)]

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pg_synapse_core::Tool;
use pg_synapse_core::error::ToolError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{ToolCtx, ToolOutput, ToolSchema};
use reqwest::Client;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

/// Tool name, as advertised to the agent.
const TOOL_NAME: &str = "get_schema_context";

/// Shared HTTP client. Lazily built on first use with a 30 s default timeout
/// so a slow pg-lexicon cannot pin an executor's tokio task indefinitely.
fn http() -> &'static Client {
    static CLIENT: std::sync::OnceLock<Client> = std::sync::OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .expect("reqwest client builds with default config")
    })
}

/// Default pg-lexicon connection name.
fn default_connection() -> String {
    "target_ecommerce".to_string()
}

/// Default database schema to scope context to.
fn default_schema() -> String {
    "ecommerce".to_string()
}

/// Default token budget for the returned context package.
fn default_budget() -> u32 {
    4000
}

/// Arguments accepted by [`GetSchemaContextTool`].
///
/// Only `question` is required; the other fields fall back to the target
/// e-commerce demo defaults so a minimal `{ "question": "..." }` call works.
#[derive(JsonSchema, Deserialize)]
struct GetSchemaContextArgs {
    /// Natural-language question to ground against the database schema.
    question: String,
    /// pg-lexicon connection name to query. Defaults to `target_ecommerce`.
    #[serde(default = "default_connection")]
    connection: String,
    /// Database schema to scope the context to. Defaults to `ecommerce`.
    #[serde(default = "default_schema")]
    schema: String,
    /// Token budget for the returned context package. Defaults to `4000`.
    #[serde(default = "default_budget")]
    budget: u32,
}

/// `get_schema_context` tool: fetches a schema-grounded NL2SQL context package
/// from a pg-lexicon service. Configured with the service `base_url` and an
/// optional bearer `token`.
pub struct GetSchemaContextTool {
    /// pg-lexicon service root, e.g. `http://127.0.0.1:9777`.
    base_url: String,
    /// Optional bearer token sent as `Authorization: Bearer {token}`.
    token: Option<String>,
}

#[async_trait]
impl Tool for GetSchemaContextTool {
    fn name(&self) -> &str {
        TOOL_NAME
    }

    fn schema(&self) -> &ToolSchema {
        static S: std::sync::OnceLock<ToolSchema> = std::sync::OnceLock::new();
        S.get_or_init(|| {
            let root = schemars::schema_for!(GetSchemaContextArgs);
            ToolSchema::from_root(root)
        })
    }

    async fn run(&self, input: Value, _ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        let args: GetSchemaContextArgs =
            serde_json::from_value(input).map_err(|e| ToolError::InvalidInput {
                name: TOOL_NAME.into(),
                reason: e.to_string(),
            })?;

        let url = format!(
            "{}/v1/context-packages",
            self.base_url.trim_end_matches('/')
        );
        let body = serde_json::json!({
            "connection": args.connection,
            "schema": args.schema,
            "question": args.question,
            "budget": args.budget,
        });

        let mut req = http().post(&url).json(&body);
        if let Some(token) = &self.token {
            req = req.header("Authorization", format!("Bearer {token}"));
        }

        let resp = req.send().await.map_err(|e| ToolError::Execution {
            name: TOOL_NAME.into(),
            reason: e.to_string(),
        })?;

        let status = resp.status();
        let text = resp.text().await.map_err(|e| ToolError::Execution {
            name: TOOL_NAME.into(),
            reason: e.to_string(),
        })?;

        if !status.is_success() {
            return Err(ToolError::Execution {
                name: TOOL_NAME.into(),
                reason: format!("pg-lexicon returned HTTP {}: {}", status.as_u16(), text),
            });
        }

        let json: Value = serde_json::from_str(&text).map_err(|e| ToolError::Execution {
            name: TOOL_NAME.into(),
            reason: format!("invalid JSON from pg-lexicon: {e}: {text}"),
        })?;

        Ok(ToolOutput::Json(json))
    }
}

/// Plugin that registers `get_schema_context` against a host's [`Registry`].
///
/// Holds the pg-lexicon service `base_url` and an optional bearer `token`,
/// both threaded into the single registered tool.
pub struct LexiconToolsPlugin {
    /// pg-lexicon service root, e.g. `http://127.0.0.1:9777`.
    pub base_url: String,
    /// Optional bearer token for the pg-lexicon service.
    pub token: Option<String>,
}

impl LexiconToolsPlugin {
    /// Construct a plugin bound to `base_url` with an optional `token`.
    pub fn new(base_url: impl Into<String>, token: Option<String>) -> Self {
        Self {
            base_url: base_url.into(),
            token,
        }
    }
}

impl Plugin for LexiconToolsPlugin {
    fn name(&self) -> &str {
        "pg-synapse-tools-lexicon"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.tools.add_arc(
            TOOL_NAME.to_string(),
            Arc::new(GetSchemaContextTool {
                base_url: self.base_url,
                token: self.token,
            }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plugin_register_inserts_the_tool() {
        let mut reg = Registry::new();
        LexiconToolsPlugin::new("http://127.0.0.1:9777", None).register(&mut reg);
        assert_eq!(reg.tools.names(), vec![TOOL_NAME]);
    }

    #[test]
    fn plugin_metadata_present() {
        let p = LexiconToolsPlugin::new("http://127.0.0.1:9777", Some("t".into()));
        assert_eq!(p.name(), "pg-synapse-tools-lexicon");
        assert!(!p.version().is_empty());
    }

    #[test]
    fn args_apply_defaults() {
        let a: GetSchemaContextArgs =
            serde_json::from_str(r#"{"question":"how many orders?"}"#).unwrap();
        assert_eq!(a.question, "how many orders?");
        assert_eq!(a.connection, "target_ecommerce");
        assert_eq!(a.schema, "ecommerce");
        assert_eq!(a.budget, 4000);
    }

    #[test]
    fn args_accept_overrides() {
        let a: GetSchemaContextArgs =
            serde_json::from_str(r#"{"question":"q","connection":"c","schema":"s","budget":100}"#)
                .unwrap();
        assert_eq!(a.connection, "c");
        assert_eq!(a.schema, "s");
        assert_eq!(a.budget, 100);
    }
}
