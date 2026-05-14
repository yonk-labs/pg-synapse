//! [`LlmProviderFactory`] + [`Plugin`] glue for [`crate::OpenAiProvider`].
//!
//! Profile-row keys consumed (under `params` unless otherwise noted):
//!
//! | Key                              | Meaning                                                                |
//! |----------------------------------|------------------------------------------------------------------------|
//! | `base_url` (profile column)      | Endpoint root, e.g. `http://192.168.1.193:8000/v1`. Wins over params.  |
//! | `params.base_url`                | Fallback endpoint root if the column is null.                          |
//! | `params._resolved_api_key`       | API key resolved from `api_key_secret` by the host. Wins over raw.     |
//! | `params.api_key`                 | Raw API key (only used when no secret was resolved).                   |
//! | `params.cost_per_million_tokens_in`  | Prompt-token price for `Usage.cost_usd` accounting.                |
//! | `params.cost_per_million_tokens_out` | Completion-token price.                                            |
//! | `params.extra_headers`           | Object of extra HTTP headers (e.g. `OpenAI-Organization`).             |
//!
//! If both the column and `params.base_url` are unset the factory defaults to
//! `https://api.openai.com/v1`.

use std::sync::Arc;

use pg_synapse_core::error::ProviderError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::LlmProfileRow;
use pg_synapse_core::{LlmProvider, LlmProviderFactory};

use crate::client::OpenAiProvider;

/// Factory + Plugin for the OpenAI / OpenAI-compatible provider.
#[derive(Default)]
pub struct OpenAiProviderFactory;

impl LlmProviderFactory for OpenAiProviderFactory {
    fn provider_name(&self) -> &str {
        "openai"
    }

    fn build(&self, profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError> {
        // base_url precedence: profile column > params.base_url > OpenAI default.
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
            .unwrap_or_else(|| "https://api.openai.com/v1".to_string());

        // API key precedence: _resolved_api_key (host-injected) > raw api_key.
        let api_key = profile
            .params
            .get("_resolved_api_key")
            .and_then(|v| v.as_str())
            .or_else(|| profile.params.get("api_key").and_then(|v| v.as_str()))
            .map(String::from);

        let cost_in = profile
            .params
            .get("cost_per_million_tokens_in")
            .and_then(|v| v.as_f64());
        let cost_out = profile
            .params
            .get("cost_per_million_tokens_out")
            .and_then(|v| v.as_f64());

        let mut p =
            OpenAiProvider::new(profile.model.clone(), base_url).with_cost(cost_in, cost_out);
        if let Some(k) = api_key {
            p = p.with_api_key(k);
        }

        if let Some(obj) = profile
            .params
            .get("extra_headers")
            .and_then(|v| v.as_object())
        {
            for (k, v) in obj {
                if let Some(sv) = v.as_str() {
                    p = p.with_extra_header(k.clone(), sv.to_string());
                }
            }
        }

        Ok(Arc::new(p))
    }
}

impl Plugin for OpenAiProviderFactory {
    fn name(&self) -> &str {
        "pg-synapse-provider-openai"
    }
    fn version(&self) -> &str {
        env!("CARGO_PKG_VERSION")
    }
    fn register(self, registry: &mut Registry) {
        registry.llm_factories.add(Arc::new(self));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_is_openai() {
        let f = OpenAiProviderFactory;
        assert_eq!(f.provider_name(), "openai");
    }

    #[test]
    fn factory_builds_with_base_url_column() {
        let f = OpenAiProviderFactory;
        let profile = LlmProfileRow {
            name: "vllm".into(),
            provider: "openai".into(),
            model: "granite-3.1-2b-instruct".into(),
            api_key_secret: None,
            base_url: Some("http://192.168.1.193:8000/v1".into()),
            params: serde_json::json!({}),
        };
        let p = f.build(profile).expect("build succeeds");
        assert_eq!(p.model_name(), "granite-3.1-2b-instruct");
    }

    #[test]
    fn factory_falls_back_to_params_base_url() {
        let f = OpenAiProviderFactory;
        let profile = LlmProfileRow {
            name: "p".into(),
            provider: "openai".into(),
            model: "m".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({"base_url": "http://local/v1"}),
        };
        let p = f.build(profile).expect("build succeeds");
        assert_eq!(p.model_name(), "m");
    }

    #[test]
    fn plugin_registers_factory_under_openai_name() {
        let mut r = Registry::new();
        OpenAiProviderFactory.register(&mut r);
        assert!(r.llm_factories.contains("openai"));
        assert_eq!(r.llm_factories.len(), 1);
    }
}
