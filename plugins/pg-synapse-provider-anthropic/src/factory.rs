//! [`LlmProviderFactory`] + [`Plugin`] glue for [`crate::AnthropicProvider`].
//!
//! Profile-row keys consumed (under `params` unless otherwise noted):
//!
//! | Key                          | Meaning                                                              |
//! |------------------------------|----------------------------------------------------------------------|
//! | `base_url` (profile column)  | Endpoint root, e.g. `https://proxy.example.com`. Wins over params.  |
//! | `params.base_url`            | Fallback endpoint root if the column is null.                        |
//! | `params._resolved_api_key`   | API key resolved from `api_key_secret` by the host. Wins over raw.  |
//! | `params.api_key`             | Raw API key (only used when no secret was resolved).                 |
//! | `params.prompt_caching`      | Boolean. When true, attaches `cache_control: ephemeral` to system.  |
//!
//! If both the profile column and `params.base_url` are unset the factory
//! defaults to `https://api.anthropic.com`.

use std::sync::Arc;

use pg_synapse_core::error::ProviderError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::LlmProfileRow;
use pg_synapse_core::{LlmProvider, LlmProviderFactory};

use crate::client::AnthropicProvider;

/// Factory + Plugin for the Anthropic Messages API provider.
#[derive(Default)]
pub struct AnthropicProviderFactory;

impl LlmProviderFactory for AnthropicProviderFactory {
    fn provider_name(&self) -> &str {
        "anthropic"
    }

    fn build(&self, profile: LlmProfileRow) -> Result<Arc<dyn LlmProvider>, ProviderError> {
        // base_url precedence: profile column > params.base_url > Anthropic default.
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
            .unwrap_or_else(|| "https://api.anthropic.com".to_string());

        // API key precedence: _resolved_api_key (host-injected) > raw api_key.
        let api_key = profile
            .params
            .get("_resolved_api_key")
            .and_then(|v| v.as_str())
            .or_else(|| profile.params.get("api_key").and_then(|v| v.as_str()))
            .map(String::from);

        let prompt_caching = profile
            .params
            .get("prompt_caching")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let mut p = AnthropicProvider::new(profile.model.clone(), base_url)
            .with_prompt_caching(prompt_caching);

        if let Some(k) = api_key {
            p = p.with_api_key(k);
        }

        Ok(Arc::new(p))
    }
}

impl Plugin for AnthropicProviderFactory {
    fn name(&self) -> &str {
        "pg-synapse-provider-anthropic"
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
    fn provider_name_is_anthropic() {
        let f = AnthropicProviderFactory;
        assert_eq!(f.provider_name(), "anthropic");
    }

    #[test]
    fn factory_builds_with_base_url_column() {
        let f = AnthropicProviderFactory;
        let profile = LlmProfileRow {
            name: "claude-proxy".into(),
            provider: "anthropic".into(),
            model: "claude-3-5-haiku-20241022".into(),
            api_key_secret: None,
            base_url: Some("https://proxy.example.com".into()),
            params: serde_json::json!({}),
        };
        let p = f.build(profile).expect("build succeeds");
        assert_eq!(p.model_name(), "claude-3-5-haiku-20241022");
    }

    #[test]
    fn factory_falls_back_to_params_base_url() {
        let f = AnthropicProviderFactory;
        let profile = LlmProfileRow {
            name: "p".into(),
            provider: "anthropic".into(),
            model: "claude-3-5-haiku-20241022".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({"base_url": "https://my-proxy.example.com"}),
        };
        let p = f.build(profile).expect("build succeeds");
        assert_eq!(p.model_name(), "claude-3-5-haiku-20241022");
    }

    #[test]
    fn factory_defaults_to_anthropic_dot_com_when_no_url() {
        let f = AnthropicProviderFactory;
        let profile = LlmProfileRow {
            name: "default".into(),
            provider: "anthropic".into(),
            model: "claude-3-5-haiku-20241022".into(),
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({}),
        };
        // Build succeeds even without an explicit URL.
        let _p = f.build(profile).expect("build succeeds with default URL");
    }

    #[test]
    fn plugin_registers_factory_under_anthropic_name() {
        let mut r = Registry::new();
        AnthropicProviderFactory.register(&mut r);
        assert!(r.llm_factories.contains("anthropic"));
        assert_eq!(r.llm_factories.len(), 1);
    }
}
