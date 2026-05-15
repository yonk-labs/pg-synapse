//! [`LlmProviderFactory`] and [`EmbeddingProviderFactory`] glue for the
//! llama-cpp provider.
//!
//! ## Profile keys consumed by [`LlamaCppProviderFactory`]
//!
//! | Key                        | Meaning                                              |
//! |----------------------------|------------------------------------------------------|
//! | `base_url` (column)        | Endpoint root. Wins over `params.base_url`.          |
//! | `params.base_url`          | Fallback endpoint if the column is null.             |
//! | `params._resolved_api_key` | API key injected by host. Wins over raw `api_key`.   |
//! | `params.api_key`           | Raw API key (used only when no secret was resolved). |
//!
//! If both the column and `params.base_url` are unset, the factory defaults to
//! `http://127.0.0.1:8080/v1` (the llama-server default).
//!
//! ## Profile keys consumed by [`LlamaCppEmbeddingFactory`]
//!
//! Same base_url precedence as above. Also reads `dimension` from the
//! `EmbeddingProfileRow` struct field (not from params).

use std::sync::Arc;

use pg_synapse_core::EmbeddingProviderFactory;
use pg_synapse_core::error::ProviderError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::{EmbeddingProfileRow, LlmProfileRow};
use pg_synapse_core::{LlmProvider, LlmProviderFactory};

use crate::client::{DEFAULT_BASE_URL, LlamaCppEmbeddingProvider, LlamaCppProvider};

// ---------------------------------------------------------------------------
// LlamaCppProviderFactory
// ---------------------------------------------------------------------------

/// Factory + Plugin for the llama-cpp LLM provider.
#[derive(Default)]
pub struct LlamaCppProviderFactory;

impl LlmProviderFactory for LlamaCppProviderFactory {
    fn provider_name(&self) -> &str {
        "llama-cpp"
    }

    fn build(&self, profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError> {
        // base_url precedence: profile column > params.base_url > default.
        let base_url = profile
            .base_url
            .clone()
            .or_else(|| {
                profile
                    .params
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        // API key precedence: _resolved_api_key > raw api_key.
        let api_key = profile
            .params
            .get("_resolved_api_key")
            .and_then(|v| v.as_str())
            .or_else(|| profile.params.get("api_key").and_then(|v| v.as_str()))
            .map(String::from);

        let mut p = LlamaCppProvider::new(profile.model.clone(), base_url);
        if let Some(k) = api_key {
            p = p.with_api_key(k);
        }

        Ok(Arc::new(p))
    }
}

impl Plugin for LlamaCppProviderFactory {
    fn name(&self) -> &str {
        "pg-synapse-provider-llama-cpp"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.llm_factories.add(Arc::new(self));
    }
}

// ---------------------------------------------------------------------------
// LlamaCppEmbeddingFactory
// ---------------------------------------------------------------------------

/// Factory + Plugin for the llama-cpp embedding provider.
#[derive(Default)]
pub struct LlamaCppEmbeddingFactory;

impl EmbeddingProviderFactory for LlamaCppEmbeddingFactory {
    fn provider_name(&self) -> &str {
        "llama-cpp"
    }

    fn build(
        &self,
        profile: EmbeddingProfileRow,
    ) -> Result<Arc<dyn pg_synapse_core::EmbeddingProvider>, ProviderError> {
        let base_url = profile
            .base_url
            .clone()
            .or_else(|| {
                profile
                    .params
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .map(String::from)
            })
            .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());

        let api_key = profile
            .params
            .get("_resolved_api_key")
            .and_then(|v| v.as_str())
            .or_else(|| profile.params.get("api_key").and_then(|v| v.as_str()))
            .map(String::from);

        let dimension = profile.dimension as usize;
        let mut p = LlamaCppEmbeddingProvider::new(profile.model.clone(), base_url, dimension);
        if let Some(k) = api_key {
            p = p.with_api_key(k);
        }

        Ok(Arc::new(p))
    }
}

impl Plugin for LlamaCppEmbeddingFactory {
    fn name(&self) -> &str {
        "pg-synapse-provider-llama-cpp-embeddings"
    }

    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }

    fn register(self, registry: &mut Registry) {
        registry.embedding_factories.add(Arc::new(self));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llm_factory_provider_name_is_llama_cpp() {
        assert_eq!(LlamaCppProviderFactory.provider_name(), "llama-cpp");
    }

    #[test]
    fn llm_factory_builds_with_base_url_column() {
        let f = LlamaCppProviderFactory;
        let profile = LlmProfileRow {
            name: "local".into(),
            provider: "llama-cpp".into(),
            model: "granite-3.0-2b-instruct".into(),
            api_key_secret: None,
            base_url: Some("http://127.0.0.1:9090/v1".into()),
            params: serde_json::json!({}),
        };
        let p = f.build(profile).expect("build ok");
        assert_eq!(p.model_name(), "granite-3.0-2b-instruct");
    }

    #[test]
    fn llm_factory_falls_back_to_params_base_url() {
        let f = LlamaCppProviderFactory;
        let profile = LlmProfileRow {
            name: "p".into(),
            provider: "llama-cpp".into(),
            model: "m".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({"base_url": "http://local/v1"}),
        };
        let p = f.build(profile).expect("build ok");
        assert_eq!(p.model_name(), "m");
    }

    #[test]
    fn llm_factory_uses_default_base_url_when_unset() {
        let f = LlamaCppProviderFactory;
        let profile = LlmProfileRow {
            name: "p".into(),
            provider: "llama-cpp".into(),
            model: "granite".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({}),
        };
        let p = f.build(profile).expect("build ok");
        assert_eq!(p.model_name(), "granite");
    }

    #[test]
    fn embedding_factory_provider_name_is_llama_cpp() {
        assert_eq!(LlamaCppEmbeddingFactory.provider_name(), "llama-cpp");
    }

    #[test]
    fn embedding_factory_builds_with_dimension_from_profile() {
        let f = LlamaCppEmbeddingFactory;
        let profile = EmbeddingProfileRow {
            name: "local-embed".into(),
            provider: "llama-cpp".into(),
            model: "nomic-embed-text".into(),
            dimension: 768,
            api_key_secret: None,
            base_url: Some("http://127.0.0.1:8080/v1".into()),
            params: serde_json::json!({}),
        };
        let p = f.build(profile).expect("build ok");
        assert_eq!(p.dimension(), 768);
        assert_eq!(p.model_name(), "nomic-embed-text");
    }

    #[test]
    fn plugin_registers_llm_factory() {
        let mut r = Registry::new();
        LlamaCppProviderFactory.register(&mut r);
        assert!(r.llm_factories.contains("llama-cpp"));
        assert_eq!(r.llm_factories.len(), 1);
    }

    #[test]
    fn plugin_registers_embedding_factory() {
        let mut r = Registry::new();
        LlamaCppEmbeddingFactory.register(&mut r);
        assert!(r.embedding_factories.contains("llama-cpp"));
        assert_eq!(r.embedding_factories.len(), 1);
    }
}
