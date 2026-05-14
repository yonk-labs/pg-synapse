//! The [`OrtEmbeddingProvider`]: an [`EmbeddingProvider`] backed by a
//! Hugging Face tokenizer and an ONNX Runtime [`Session`].
//!
//! Input contract:
//!
//! - The ONNX graph must accept `input_ids`, `attention_mask`, and (optionally)
//!   `token_type_ids`, each shaped `(batch, seq_len)` with `i64` element type.
//!   BGE, MiniLM, and E5 exports from Hugging Face all match this layout.
//! - The first output is interpreted as `last_hidden_state` with shape
//!   `(batch, seq_len, hidden)` and `f32` element type. We mean-pool over the
//!   sequence axis (mask-aware) and L2-normalize, matching the sentence
//!   embedding convention used by the BGE family.
//!
//! [`Session`]: ort::session::Session

use std::path::Path;

use async_trait::async_trait;
use ndarray::{Array2, Ix3};
use ort::{
    session::{Session, builder::GraphOptimizationLevel},
    value::Tensor,
};
use parking_lot::Mutex;
use tokenizers::Tokenizer;

use pg_synapse_core::EmbeddingProvider;
use pg_synapse_core::error::EmbeddingError;
use pg_synapse_core::types::EmbeddingVector;

/// Default sequence-length cap when none is configured.
pub const DEFAULT_MAX_SEQ_LEN: usize = 512;

/// Provider that turns text into BGE-style embeddings via ONNX Runtime.
///
/// The session is held behind a `Mutex` because `Session::run` takes
/// `&mut self`. Embedding throughput is dominated by batched matmul inside ORT
/// so the mutex contention does not become a bottleneck before saturating
/// the underlying CPU/GPU.
pub struct OrtEmbeddingProvider {
    session: Mutex<Session>,
    tokenizer: Tokenizer,
    model_name: String,
    dimension: usize,
    max_seq_len: usize,
    has_token_type_ids: bool,
}

impl OrtEmbeddingProvider {
    /// Construct a provider by loading an ONNX graph and `tokenizer.json` from
    /// disk.
    ///
    /// `intra_threads` controls ORT's intra-op thread pool; `None` leaves it at
    /// the ORT default (one thread per logical core, capped by ORT).
    pub fn new(
        model_path: &Path,
        tokenizer_path: &Path,
        model_name: impl Into<String>,
        dimension: usize,
        max_seq_len: usize,
        intra_threads: Option<usize>,
    ) -> Result<Self, EmbeddingError> {
        let mut builder = Session::builder()
            .map_err(|e| EmbeddingError::Provider(format!("ort session builder: {e}")))?
            .with_optimization_level(GraphOptimizationLevel::Level3)
            .map_err(|e| EmbeddingError::Provider(format!("ort optimization level: {e}")))?;

        if let Some(n) = intra_threads {
            builder = builder
                .with_intra_threads(n)
                .map_err(|e| EmbeddingError::Provider(format!("ort intra_threads: {e}")))?;
        }

        let session = builder.commit_from_file(model_path).map_err(|e| {
            EmbeddingError::Provider(format!("ort load {}: {e}", model_path.display()))
        })?;

        // Detect whether the graph takes a token_type_ids input. BGE ONNX
        // exports normally do; some MiniLM exports do not.
        let has_token_type_ids = session
            .inputs()
            .iter()
            .any(|i| i.name() == "token_type_ids");

        let tokenizer = Tokenizer::from_file(tokenizer_path)
            .map_err(|e| EmbeddingError::Provider(format!("tokenizer load: {e}")))?;

        Ok(Self {
            session: Mutex::new(session),
            tokenizer,
            model_name: model_name.into(),
            dimension,
            max_seq_len: max_seq_len.max(1),
            has_token_type_ids,
        })
    }
}

#[async_trait]
impl EmbeddingProvider for OrtEmbeddingProvider {
    async fn embed(&self, texts: &[&str]) -> Result<Vec<EmbeddingVector>, EmbeddingError> {
        if texts.is_empty() {
            return Ok(vec![]);
        }

        // Tokenize the whole batch.
        let owned: Vec<String> = texts.iter().map(|s| (*s).to_string()).collect();
        let encodings = self
            .tokenizer
            .encode_batch(owned, true)
            .map_err(|e| EmbeddingError::Provider(format!("tokenize: {e}")))?;

        let raw_max = encodings.iter().map(|e| e.len()).max().unwrap_or(1);
        let max_len = raw_max.min(self.max_seq_len).max(1);
        let batch = encodings.len();

        let mut ids = vec![0i64; batch * max_len];
        let mut mask = vec![0i64; batch * max_len];
        let mut types = vec![0i64; batch * max_len];

        for (i, enc) in encodings.iter().enumerate() {
            let n = enc.len().min(max_len);
            let row = i * max_len;
            for j in 0..n {
                ids[row + j] = enc.get_ids()[j] as i64;
                mask[row + j] = enc.get_attention_mask()[j] as i64;
                types[row + j] = enc.get_type_ids()[j] as i64;
            }
        }

        let ids_arr = Array2::from_shape_vec((batch, max_len), ids)
            .map_err(|e| EmbeddingError::Provider(format!("ids shape: {e}")))?;
        let mask_arr = Array2::from_shape_vec((batch, max_len), mask)
            .map_err(|e| EmbeddingError::Provider(format!("mask shape: {e}")))?;
        let types_arr = Array2::from_shape_vec((batch, max_len), types)
            .map_err(|e| EmbeddingError::Provider(format!("types shape: {e}")))?;

        // Clone mask before moving into the tensor so we can use it during
        // pooling.
        let mask_for_pool = mask_arr.clone();

        let ids_tensor = Tensor::from_array(ids_arr)
            .map_err(|e| EmbeddingError::Provider(format!("ids tensor: {e}")))?;
        let mask_tensor = Tensor::from_array(mask_arr)
            .map_err(|e| EmbeddingError::Provider(format!("mask tensor: {e}")))?;

        let mut sess = self.session.lock();
        let outputs = if self.has_token_type_ids {
            let types_tensor = Tensor::from_array(types_arr)
                .map_err(|e| EmbeddingError::Provider(format!("types tensor: {e}")))?;
            sess.run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
                "token_type_ids" => types_tensor,
            ])
        } else {
            sess.run(ort::inputs![
                "input_ids" => ids_tensor,
                "attention_mask" => mask_tensor,
            ])
        }
        .map_err(|e| EmbeddingError::Provider(format!("ort run: {e}")))?;

        // First output = last_hidden_state, shape (batch, seq, hidden).
        let view = outputs[0]
            .try_extract_array::<f32>()
            .map_err(|e| EmbeddingError::Provider(format!("extract output: {e}")))?;

        let shape = view.shape().to_vec();
        if shape.len() != 3 {
            return Err(EmbeddingError::Provider(format!(
                "unexpected output rank: {shape:?}"
            )));
        }
        let hidden = shape[2];
        if hidden != self.dimension {
            return Err(EmbeddingError::DimensionMismatch {
                expected: self.dimension,
                actual: hidden,
            });
        }

        // Mean-pool + L2 normalize.
        let arr3 = view
            .into_dimensionality::<Ix3>()
            .map_err(|e| EmbeddingError::Provider(format!("cast last_hidden_state to 3-D: {e}")))?;

        let mut result = Vec::with_capacity(batch);
        for b in 0..batch {
            let mut sum = vec![0f32; hidden];
            let mut count = 0f32;
            for s in 0..max_len {
                if mask_for_pool[[b, s]] == 1 {
                    for (h, slot) in sum.iter_mut().enumerate().take(hidden) {
                        *slot += arr3[[b, s, h]];
                    }
                    count += 1.0;
                }
            }
            if count > 0.0 {
                for v in sum.iter_mut() {
                    *v /= count;
                }
            }
            let norm: f32 = sum.iter().map(|v| v * v).sum::<f32>().sqrt().max(1e-12);
            for v in sum.iter_mut() {
                *v /= norm;
            }
            result.push(EmbeddingVector::from(sum));
        }

        Ok(result)
    }

    fn dimension(&self) -> usize {
        self.dimension
    }

    fn model_name(&self) -> &str {
        &self.model_name
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Construction touches the ONNX Runtime dylib (load-dynamic feature), so a
    // direct unit test of `new()` is environment-dependent. We exercise the
    // valid path through the factory-level integration test under
    // `live-tests`, and assert the "bad params" surface at the factory layer
    // (which short-circuits before touching ORT).
    #[test]
    fn default_max_seq_len_matches_bge() {
        assert_eq!(DEFAULT_MAX_SEQ_LEN, 512);
    }
}
