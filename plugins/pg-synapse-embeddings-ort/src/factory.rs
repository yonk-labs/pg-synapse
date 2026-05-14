//! [`EmbeddingProviderFactory`] + [`Plugin`] glue for [`OrtEmbeddingProvider`].
//!
//! Profile-row keys consumed from `params`:
//!
//! | Key                | Required | Meaning                                                      |
//! |--------------------|----------|--------------------------------------------------------------|
//! | `model_path`       | yes      | Filesystem path to the `.onnx` graph.                        |
//! | `tokenizer_path`   | yes      | Filesystem path to `tokenizer.json`.                         |
//! | `max_seq_len`      | no       | Sequence-length cap (default `512`).                         |
//! | `intra_threads`    | no       | ORT intra-op thread count (default: ORT default).            |

use std::sync::Arc;

use pg_synapse_core::EmbeddingProvider;
use pg_synapse_core::EmbeddingProviderFactory;
use pg_synapse_core::error::ProviderError;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::EmbeddingProfileRow;

use crate::provider::{DEFAULT_MAX_SEQ_LEN, OrtEmbeddingProvider};

/// Factory + Plugin for ORT-backed embeddings (BGE / MiniLM / E5).
#[derive(Default)]
pub struct OrtEmbeddingFactory;

impl EmbeddingProviderFactory for OrtEmbeddingFactory {
    fn provider_name(&self) -> &str {
        "ort"
    }

    fn build(
        &self,
        profile: EmbeddingProfileRow,
    ) -> Result<Arc<dyn EmbeddingProvider>, ProviderError> {
        let model_path = profile
            .params
            .get("model_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidProfile {
                provider: "ort".into(),
                reason: "missing params.model_path".into(),
            })?;
        let tokenizer_path = profile
            .params
            .get("tokenizer_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| ProviderError::InvalidProfile {
                provider: "ort".into(),
                reason: "missing params.tokenizer_path".into(),
            })?;
        let max_seq_len = profile
            .params
            .get("max_seq_len")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_SEQ_LEN);
        let intra_threads = profile
            .params
            .get("intra_threads")
            .and_then(|v| v.as_u64())
            .map(|n| n as usize);

        let provider = OrtEmbeddingProvider::new(
            std::path::Path::new(model_path),
            std::path::Path::new(tokenizer_path),
            profile.model.clone(),
            profile.dimension as usize,
            max_seq_len,
            intra_threads,
        )
        .map_err(|e| ProviderError::InvalidProfile {
            provider: "ort".into(),
            reason: e.to_string(),
        })?;

        Ok(Arc::new(provider))
    }
}

impl Plugin for OrtEmbeddingFactory {
    fn name(&self) -> &str {
        "pg-synapse-embeddings-ort"
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
    fn provider_name_is_ort() {
        let f = OrtEmbeddingFactory;
        assert_eq!(f.provider_name(), "ort");
    }

    #[test]
    fn factory_rejects_missing_model_path() {
        let f = OrtEmbeddingFactory;
        let profile = EmbeddingProfileRow {
            name: "bad".into(),
            provider: "ort".into(),
            model: "BAAI/bge-small-en-v1.5".into(),
            dimension: 384,
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({ "tokenizer_path": "/tmp/tok.json" }),
        };
        let err = match f.build(profile) {
            Ok(_) => panic!("expected ProviderError"),
            Err(e) => e,
        };
        let msg = format!("{err:?}");
        assert!(msg.contains("model_path"), "got: {msg}");
    }

    #[test]
    fn factory_rejects_missing_tokenizer_path() {
        let f = OrtEmbeddingFactory;
        let profile = EmbeddingProfileRow {
            name: "bad".into(),
            provider: "ort".into(),
            model: "BAAI/bge-small-en-v1.5".into(),
            dimension: 384,
            api_key_secret: None,
            base_url: None,
            params: serde_json::json!({ "model_path": "/tmp/model.onnx" }),
        };
        let err = match f.build(profile) {
            Ok(_) => panic!("expected ProviderError"),
            Err(e) => e,
        };
        let msg = format!("{err:?}");
        assert!(msg.contains("tokenizer_path"), "got: {msg}");
    }

    #[test]
    fn plugin_registers_factory_under_ort_name() {
        let mut r = Registry::new();
        OrtEmbeddingFactory.register(&mut r);
        assert!(r.embedding_factories.contains("ort"));
        assert_eq!(r.embedding_factories.len(), 1);
    }
}
