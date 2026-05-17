//! Profile-row data types: serde-shaped views of `pg_synapse.llm_profiles`,
//! `pg_synapse.embedding_profiles`, and `pg_synapse.agents`.
//!
//! Host crates (`pg-synapse-pgrx`, `pg-synapse-sidecar`) read these rows from
//! Postgres and pass them into the kernel; provider factory plugins consume
//! them via [`crate::LlmProviderFactory::build`] and
//! [`crate::EmbeddingProviderFactory::build`].

use serde::{Deserialize, Serialize};

/// One row in `pg_synapse.llm_profiles`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct LlmProfileRow {
    /// Unique name (primary key).
    pub name: String,
    /// Provider this profile binds to (e.g. `"openai"`, `"anthropic"`).
    pub provider: String,
    /// Model identifier (e.g. `"gpt-4o"`).
    pub model: String,
    /// Name of a row in `pg_synapse.secrets` holding the API key. None for local providers.
    pub api_key_secret: Option<String>,
    /// Override URL (None means use the provider's default endpoint).
    pub base_url: Option<String>,
    /// Free-form provider-specific knobs (temperature defaults, max_tokens, ...).
    pub params: serde_json::Value,
}

/// One row in `pg_synapse.embedding_profiles`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct EmbeddingProfileRow {
    /// Unique name (primary key).
    pub name: String,
    /// Embedding provider (e.g. `"openai"`, `"ort"`, `"llama-cpp"`).
    pub provider: String,
    /// Model identifier (e.g. `"text-embedding-3-small"`).
    pub model: String,
    /// Vector dimension declared for the model.
    pub dimension: u32,
    /// Secret-name reference for cloud providers; None for local.
    pub api_key_secret: Option<String>,
    /// Override URL (None means provider default).
    pub base_url: Option<String>,
    /// Free-form provider-specific knobs.
    pub params: serde_json::Value,
}

/// One row in `pg_synapse.agents`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct AgentRow {
    /// Unique agent name.
    pub name: String,
    /// System prompt prepended to every turn.
    pub system_prompt: String,
    /// Optional extended persona block (the "soul").
    pub soul: Option<String>,
    /// Which executor implementation to use (e.g. `"conversation"`, `"react"`).
    pub executor_name: String,
    /// Name of the `main`-role LLM profile.
    pub llm_profile_main: Option<String>,
    /// Name of the `small`-role LLM profile (classifier / coercion).
    pub llm_profile_small: Option<String>,
    /// Name of the `judge`-role LLM profile (reflection critique).
    pub llm_profile_judge: Option<String>,
    /// Embedding profile name (for RAG-shaped tools).
    pub embedding_profile: Option<String>,
    /// Names of tools this agent is allowed to call.
    pub tools: Vec<String>,
    /// Per-execution iteration cap.
    pub max_iterations: u32,
    /// Per-execution wall-clock timeout in milliseconds.
    pub timeout_ms: u64,
    /// Per-execution USD cost cap. None means no cap.
    pub cost_cap_usd: Option<f64>,
    /// Per-agent trace-level override. None means inherit the global GUC.
    pub trace_level: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_profile_row_roundtrips() {
        let p = LlmProfileRow {
            name: "main".into(),
            provider: "openai".into(),
            model: "gpt-4o".into(),
            api_key_secret: Some("OPENAI_KEY".into()),
            base_url: None,
            params: serde_json::json!({"temperature": 0.2}),
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: LlmProfileRow = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn embedding_profile_row_roundtrips() {
        let p = EmbeddingProfileRow {
            name: "small-embed".into(),
            provider: "openai".into(),
            model: "text-embedding-3-small".into(),
            dimension: 1536,
            api_key_secret: Some("OPENAI_KEY".into()),
            base_url: None,
            params: serde_json::Value::Null,
        };
        let s = serde_json::to_string(&p).unwrap();
        let back: EmbeddingProfileRow = serde_json::from_str(&s).unwrap();
        assert_eq!(p, back);
    }

    #[test]
    fn agent_row_roundtrips() {
        let a = AgentRow {
            name: "researcher".into(),
            system_prompt: "Be precise.".into(),
            soul: Some("You are Alice.".into()),
            executor_name: "react".into(),
            llm_profile_main: Some("main".into()),
            llm_profile_small: None,
            llm_profile_judge: None,
            embedding_profile: None,
            tools: vec!["http_get".into()],
            max_iterations: 8,
            timeout_ms: 60_000,
            cost_cap_usd: Some(0.50),
            trace_level: None,
        };
        let s = serde_json::to_string(&a).unwrap();
        let back: AgentRow = serde_json::from_str(&s).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn agent_row_default_empty() {
        let a = AgentRow::default();
        assert!(a.name.is_empty());
        assert!(a.tools.is_empty());
        assert_eq!(a.max_iterations, 0);
    }

    #[test]
    fn llm_profile_debug_includes_provider() {
        let p = LlmProfileRow {
            provider: "anthropic".into(),
            ..Default::default()
        };
        let s = format!("{:?}", p);
        assert!(s.contains("anthropic"));
    }
}
