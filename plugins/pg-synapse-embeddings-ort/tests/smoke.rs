//! Integration smoke tests for the ORT embeddings plugin.
//!
//! The live-tests group is gated by the `live-tests` feature (which itself
//! pulls in `download`). Without that feature the live tests still compile via
//! `#[ignore]`, but `cargo test` skips them by default. The plugin-shape tests
//! always run.

use std::sync::Arc;

use pg_synapse_core::EmbeddingProviderFactory;
use pg_synapse_core::plugin::{Plugin, Registry};
use pg_synapse_core::types::EmbeddingProfileRow;
use pg_synapse_embeddings_ort::OrtEmbeddingFactory;

#[test]
fn plugin_registers_as_ort_factory() {
    let mut registry = Registry::new();
    OrtEmbeddingFactory.register(&mut registry);
    let factory = registry
        .embedding_factories
        .get("ort")
        .expect("ort factory registered");
    assert_eq!(factory.provider_name(), "ort");
}

#[test]
fn factory_invalid_profile_surfaces_provider_error() {
    let f = OrtEmbeddingFactory;
    let profile = EmbeddingProfileRow {
        name: "broken".into(),
        provider: "ort".into(),
        model: "BAAI/bge-small-en-v1.5".into(),
        dimension: 384,
        api_key_secret: None,
        base_url: None,
        params: serde_json::json!({}),
    };
    let err = match f.build(profile) {
        Ok(_) => panic!("expected ProviderError for missing model_path"),
        Err(e) => e,
    };
    assert!(format!("{err:?}").contains("model_path"));
}

#[test]
fn embedding_factory_trait_object_dispatch() {
    let f: Arc<dyn EmbeddingProviderFactory> = Arc::new(OrtEmbeddingFactory);
    assert_eq!(f.provider_name(), "ort");
}

#[cfg(feature = "live-tests")]
#[tokio::test]
#[ignore = "downloads ~130MB and links against libonnxruntime.so"]
async fn embeds_sentence_with_bge_small() {
    use pg_synapse_core::EmbeddingProvider;
    use pg_synapse_embeddings_ort::OrtEmbeddingProvider;
    use pg_synapse_embeddings_ort::download::{DownloadConfig, ensure_files};

    let cfg = DownloadConfig::bge_small_en_v15();
    let (model_path, tokenizer_path) = tokio::task::spawn_blocking(move || ensure_files(&cfg))
        .await
        .expect("download task")
        .expect("download BGE small");

    let provider = OrtEmbeddingProvider::new(
        &model_path,
        &tokenizer_path,
        "BAAI/bge-small-en-v1.5",
        384,
        512,
        None,
    )
    .expect("construct ort provider");

    let vectors = provider
        .embed(&["This is a sentence.", "Another piece of text."])
        .await
        .expect("embed two sentences");

    assert_eq!(vectors.len(), 2);
    assert_eq!(vectors[0].dimension(), 384);
    assert_eq!(vectors[1].dimension(), 384);

    // L2-normalized embeddings have ~unit norm.
    for v in &vectors {
        let n: f32 = v.as_slice().iter().map(|x| x * x).sum::<f32>().sqrt();
        assert!((n - 1.0).abs() < 1e-3, "expected L2 norm near 1.0, got {n}");
    }

    // Cosine similarity is symmetric and in [-1, 1].
    let a = vectors[0].as_slice();
    let b = vectors[1].as_slice();
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    assert!(dot.is_finite());
    assert!(dot > -1.001 && dot < 1.001);
}
